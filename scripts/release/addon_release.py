"""Manifest-driven staging and addon release assets. Never package runtime data."""
import hashlib
import json
from pathlib import Path
import re
import tarfile
import tomllib
from prepare_version import release_version, replace_version

TARGETS = {"x86_64-unknown-linux-musl": ("linux-musl-amd64", ".tar.gz"),
           "aarch64-unknown-linux-musl": ("linux-musl-arm64", ".tar.gz"),
           "x86_64-unknown-linux-gnu": ("linux-amd64", ".tar.gz"),
           "aarch64-unknown-linux-gnu": ("linux-arm64", ".tar.gz"),
           "x86_64-apple-darwin": ("macos-amd64", ".dmg"),
           "aarch64-apple-darwin": ("macos-arm64", ".dmg")}
RELEASE_TARGETS = ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu")

CONTAINER_TARGETS = ("x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl")
ALL_ADDON_TARGETS = RELEASE_TARGETS + CONTAINER_TARGETS


def release_targets(manifest):
    return ALL_ADDON_TARGETS if manifest.get('execution') == 'workspace' else RELEASE_TARGETS


def build_matrix(manifest):
    return {'include': [dict(runner='ubuntu-24.04-arm' if target.startswith('aarch64') else 'ubuntu-24.04', target=target) for target in release_targets(manifest)]}


def safe_tag(tag):
    value = re.sub(r"[^A-Za-z0-9._-]", "-", tag)
    if not value:
        raise ValueError("Release tag must not be empty")
    return value


def assets(manifest):
    names = ["addon.toml", manifest["executable"], manifest["icon"], "LICENSE", "THIRD_PARTY_NOTICES.txt"]
    if len(set(names)) != len(names):
        raise ValueError("Addon asset paths overlap")
    for name in names:
        path = Path(name)
        if path.is_absolute() or not path.parts or any(p in ("..", ".") for p in path.parts):
            raise ValueError("Addon assets must stay within the package")
    return names


def manifests(root=Path(".")):
    result = []
    for path in sorted((root / "addons").glob("*/addon.toml")):
        manifest = tomllib.loads(path.read_text())
        if not re.fullmatch(r"[A-Za-z0-9_-]+", manifest["id"]) or manifest["id"] != path.parent.name:
            raise ValueError("Addon directory must match its ID")
        assets(manifest)
        result.append((path, manifest))
    return result


def archive_name(tag, target, addon_id):
    return f"focus-addon-{addon_id}-{safe_tag(tag)}-{TARGETS[target][0]}.tar.gz"


def index_name(target):
    return f"addons-{TARGETS[target][0]}.json"


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def require_editor_compatibility(manifest):
    requirement = manifest.get("editor_version_req")
    if not isinstance(requirement, str) or not requirement.strip():
        raise ValueError(f"Addon {manifest.get('id', '<unknown>')} must declare a nonempty editor_version_req for independent releases")
    return requirement


def editor_compatibility(manifest, lower='', upper=''):
    """Override either end of the release's >=lower, <upper editor range."""
    requirement = require_editor_compatibility(manifest)
    if not lower and not upper:
        return requirement
    bounds = {}
    for part in requirement.split(','):
        match = re.fullmatch(r'\s*(>=|<)\s*(\d+\.\d+\.\d+)\s*', part)
        if not match or match[1] in bounds:
            raise ValueError('Editing editor compatibility requires a >=x.y.z, <x.y.z range')
        bounds[match[1]] = match[2]
    if set(bounds) != {'>=', '<'}:
        raise ValueError('Editing editor compatibility requires a >=x.y.z, <x.y.z range')
    lower, upper = lower or bounds['>='], upper or bounds['<']
    from prepare_version import release_version
    for value in (lower, upper):
        release_version(value)
        if not re.fullmatch(r'\d+\.\d+\.\d+', value):
            raise ValueError('Editor bounds require stable x.y.z versions')
    if tuple(map(int, lower.split('.'))) >= tuple(map(int, upper.split('.'))):
        raise ValueError('Editor --from must be less than --to')
    return f'>={lower}, <{upper}'


def package_addons(binary_directory, destination, tag, target, root=Path("."), selected=None):
    destination.mkdir(parents=True, exist_ok=True)
    packages = []
    for _, definition in manifests(root):
        if selected is not None and definition["id"] not in selected:
            continue
        source = binary_directory / "addons" / definition["id"]
        manifest = tomllib.loads((source / "addon.toml").read_text())
        if manifest["target"] != target or any(manifest[key] != definition[key] for key in ("id", "version", "protocol", "executable", "icon")):
            raise ValueError("Staged addon identity/platform mismatch")
        if selected is not None:
            expected = require_editor_compatibility(definition)
            if require_editor_compatibility(manifest) != expected:
                raise ValueError(f"Addon {manifest['id']} staged editor compatibility range does not match its source manifest")
        name = archive_name(tag, target, manifest["id"])
        path = destination / name
        with tarfile.open(path, "w:gz", format=tarfile.PAX_FORMAT) as archive:
            for relative in assets(manifest):
                asset = source / relative
                if asset.is_symlink() or not asset.is_file() or not asset.resolve().is_relative_to(source.resolve()):
                    raise ValueError("Invalid addon asset")
                archive.add(asset, arcname=relative, recursive=False)
        if path.stat().st_size > 256 * 1024 * 1024:
            raise ValueError("Addon archive exceeds manager size limit")
        packages.append({"id": manifest["id"], "name": manifest["name"],
                         "description": manifest.get("description", ""), "publisher": manifest.get("publisher", ""),
                         "version": manifest["version"], "protocol": manifest["protocol"], "target": target,
                         "editor_version_req": manifest.get("editor_version_req"),
                         "archive": name, "size": path.stat().st_size, "sha256": digest(path)})
    catalog = {"schema": 2 if selected is not None else 1, "release": tag,
               "editor_version": tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"],
               "packages": packages}
    (destination / index_name(target)).write_text(json.dumps(catalog, indent=2) + "\n")
    return catalog


def stable(version):
    release_version(version)
    if not re.fullmatch(r'\d+\.\d+\.\d+', version):
        raise ValueError('Addon releases require a stable x.y.z version')
    return tuple(map(int, version.split('.')))


def next_version(current, versions, explicit=''):
    previous = max([stable(current), *(stable(v) for v in versions)])
    if explicit:
        chosen = stable(explicit)
        if chosen <= max((stable(v) for v in versions), default=(0, 0, 0)) or chosen < stable(current):
            raise ValueError('Explicit version must exceed released versions and not precede the source version')
        return explicit
    return f'{previous[0]}.{previous[1]}.{previous[2] + 1}'


def version_files(manifest_text, project_text, lock_text, version, lower='', upper=''):
    """Prepare all three metadata files without writing or resolving dependencies."""
    stable(version)
    manifest = tomllib.loads(manifest_text)
    requirement = editor_compatibility(manifest, lower, upper)
    name = tomllib.loads(project_text)['package']['name']
    sections = re.split(r'(?m)(?=^\[)', project_text)
    count = 0
    for i, section in enumerate(sections):
        if section.startswith('[package]\n'):
            sections[i] = replace_version(section, version)
            count += 1
    if count != 1:
        raise ValueError('Expected one Cargo package section')
    blocks = lock_text.split('[[package]]')
    matches = 0
    for i in range(1, len(blocks)):
        package = tomllib.loads(blocks[i])
        if package['name'] == name and 'source' not in package:
            blocks[i] = replace_version(blocks[i], version)
            matches += 1
    if matches != 1:
        raise ValueError('Expected one local package in addon lockfile')
    manifest_text = replace_version(manifest_text, version)
    if lower or upper:
        manifest_text, count = re.subn(r'(?m)^editor_version_req\s*=.*$',
                                      'editor_version_req = ' + json.dumps(requirement), manifest_text)
        if count != 1:
            raise ValueError('Expected one editor_version_req in addon manifest')
    result = {'addon.toml': manifest_text, 'Cargo.toml': ''.join(sections),
              'Cargo.lock': '[[package]]'.join(blocks)}
    for value in result.values():
        tomllib.loads(value)
    return result
