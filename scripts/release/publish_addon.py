#!/usr/bin/env python3
"""Reserve, stamp, package and publish one independently versioned addon."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tomllib
from addon_release import manifests, package_addons, RELEASE_TARGETS, index_name, digest, archive_name, editor_compatibility
from prepare_version import replace_version, release_version

ROOT = Path(__file__).resolve().parents[2]


def gh(*args):
    return subprocess.check_output(['gh', *args], text=True).strip()


def release_pages(repo, query=None):
    """Decode the consecutive JSON arrays emitted by gh api --paginate.

    Parse the stream ourselves so older GitHub CLI versions work without
    --slurp, which was added after pagination support.
    """
    output = (query or gh)('api', '--paginate', f'repos/{repo}/releases?per_page=100')
    decoder = json.JSONDecoder()
    pages = []
    offset = 0
    while offset < len(output):
        if output[offset].isspace():
            offset += 1
            continue
        page, offset = decoder.raw_decode(output, offset)
        if not isinstance(page, list):
            raise ValueError('Expected a GitHub release page to be a JSON array')
        pages.append(page)
    if not pages:
        raise ValueError('GitHub returned no release pages')
    return pages


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


def definition(addon):
    for item in manifests(ROOT):
        if item[1]['id'] == addon:
            return item
    raise ValueError(f'Unknown addon: {addon}')


def stamp(addon, tag, lower='', upper=''):
    path, manifest = definition(addon)
    requirement = editor_compatibility(manifest, lower, upper)
    prefix = f'addon_{addon}_'
    if not tag.startswith(prefix):
        raise ValueError('Tag does not match addon')
    version = tag.removeprefix(prefix)
    stable(version)
    # Stamp this package only; SDK/support and dependency versions are unchanged.
    project = path.with_name('Cargo.toml')
    name = tomllib.loads(project.read_text())['package']['name']
    sections = re.split(r'(?m)(?=^\[)', project.read_text())
    sections = [replace_version(s, version) if s.startswith('[package]\n') else s for s in sections]
    lock = project.with_name('Cargo.lock')
    blocks = lock.read_text().split('[[package]]')
    matches = 0
    for i in range(1, len(blocks)):
        package = tomllib.loads(blocks[i])
        if package['name'] == name and 'source' not in package:
            blocks[i] = replace_version(blocks[i], version)
            matches += 1
    if matches != 1:
        raise ValueError('Expected one local package in addon lockfile')
    changes = {path: replace_version(path.read_text(), version), project: ''.join(sections), lock: '[[package]]'.join(blocks)}
    if lower or upper:
        changes[path], count = re.subn(r'(?m)^editor_version_req\s*=.*$',
                                      'editor_version_req = ' + json.dumps(requirement), changes[path])
        if count != 1:
            raise ValueError('Expected one editor_version_req in addon manifest')
    for text in changes.values():
        tomllib.loads(text)
    for file, text in changes.items():
        file.write_text(text)


def fingerprint(addon, sha):
    """Hash build inputs, including transitive local Cargo dependencies."""
    from sys import path
    path.insert(0, str(ROOT / 'scripts/ci'))
    from changes import projects, local_dependencies
    available = projects(ROOT)
    selected = {addon}
    while True:
        previous = selected.copy()
        for key in list(selected):
            manifest = available[key]
            for dependency in local_dependencies(manifest):
                selected.update(k for k, p in available.items() if p.resolve() == dependency)
        if selected == previous:
            break
    inputs = [str(available[k].parent.relative_to(ROOT)) for k in sorted(selected)]
    inputs += ['scripts/addons.py', 'scripts/release', 'LICENSE', 'THIRD_PARTY_NOTICES.txt', '.github/actions/linux-checks', '.github/workflows/addon-release.yml']
    tree = subprocess.check_output(['git', 'ls-tree', '-r', sha, '--', *inputs], cwd=ROOT)
    # README edits do not change addon executables or release assets.
    tree = b'\n'.join(line for line in tree.splitlines() if not line.endswith(b'.md'))
    return hashlib.sha256(tree).hexdigest()


def reserve(addon, sha, explicit, lower='', upper=''):
    _, manifest = definition(addon)
    requirement = editor_compatibility(manifest, lower, upper)
    prefix = f'addon_{addon}_'
    pages = release_pages(os.environ['GH_REPO'])
    releases = [r for page in pages for r in page if r['tag_name'].startswith(prefix)
                and re.fullmatch(r'\d+\.\d+\.\d+', r['tag_name'][len(prefix):])]
    releases.sort(key=lambda r: stable(r['tag_name'][len(prefix):]), reverse=True)
    marker = f'Addon source: {sha}'
    content = f'Addon inputs: {fingerprint(addon, sha)}'
    compatibility = f'Editor compatibility override: {requirement}' if lower or upper else ''
    if compatibility:
        content += ':' + hashlib.sha256(requirement.encode()).hexdigest()
    if not explicit:
        latest = next((r for r in releases if not r['draft']), None)
        if latest and content in (latest.get('body') or '').splitlines():
            return latest['tag_name'], True
    matching = [r for r in releases if marker in (r.get('body') or '').splitlines()
                and (compatibility in (r.get('body') or '').splitlines() if compatibility else
                     not any(line.startswith('Editor compatibility override:') for line in (r.get('body') or '').splitlines()))
                and (not explicit or r['tag_name'] == prefix + explicit)]
    if matching:
        release = matching[0]
        return release['tag_name'], not release['draft']
    versions = [r['tag_name'][len(prefix):] for r in releases
                if re.fullmatch(r'\d+\.\d+\.\d+', r['tag_name'][len(prefix):])]
    tag = prefix + next_version(manifest['version'], versions, explicit)
    body = marker + '\n' + content + ('\n' + compatibility if compatibility else '') + '\n\nIndependent addon release. See addons.json for supported editor versions.\n'
    # gh creates the immutable source tag; draft assets are invisible to discovery.
    gh('release', 'create', tag, '--target', sha, '--draft', '--title', tag, '--notes', body, '--latest=false')
    return tag, False


def finalize(addon, tag, directory):
    _, manifest = definition(addon)
    packages = []
    expected = set()
    for target in RELEASE_TARGETS:
        name = index_name(target)
        expected.add(name)
        catalog = json.loads((directory / name).read_text())
        if catalog['schema'] != 2 or catalog['release'] != tag or len(catalog['packages']) != 1:
            raise ValueError('Invalid per-platform catalog')
        p = catalog['packages'][0]
        archive = archive_name(tag, target, addon)
        if any(p[key] != manifest[key] for key in ('id', 'version', 'protocol', 'editor_version_req')) or p['target'] != target or p['archive'] != archive:
            raise ValueError('Addon catalog identity mismatch')
        file = directory / archive
        if p['size'] != file.stat().st_size or p['sha256'] != digest(file):
            raise ValueError('Addon archive checksum mismatch')
        expected.add(archive)
        packages.append(p)
    if {p.name for p in directory.iterdir()} - {'addons.json', 'SHA256SUMS'} != expected:
        raise ValueError('Unexpected or missing addon assets')
    (directory / 'addons.json').write_text(json.dumps({'schema': 2, 'release': tag, 'packages': packages}, indent=2) + '\n')
    expected.add('addons.json')
    (directory / 'SHA256SUMS').write_text(''.join(f'{digest(directory / name)}  {name}\n' for name in sorted(expected)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['reserve', 'stamp', 'package', 'publish'])
    parser.add_argument('--addon', required=True)
    parser.add_argument('--tag')
    parser.add_argument('--source-sha')
    parser.add_argument('--version', default='')
    parser.add_argument('--target')
    parser.add_argument('--from', dest='editor_from', default='')
    parser.add_argument('--to', dest='editor_to', default='')
    args = parser.parse_args()
    definition(args.addon)
    if args.command == 'reserve':
        if not re.fullmatch(r'[0-9a-f]{40}', args.source_sha or ''):
            raise ValueError('Expected a full source commit SHA')
        tag, done = reserve(args.addon, args.source_sha, args.version, args.editor_from, args.editor_to)
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            output.write(f'tag={tag}\ndone={str(done).lower()}\n')
    else:
        stamp(args.addon, args.tag, args.editor_from, args.editor_to)
        if args.command == 'package':
            from package import verify_binary
            _, manifest = definition(args.addon)
            directory = ROOT / 'target' / args.target / 'release'
            verify_binary(directory / 'addons' / args.addon / manifest['executable'], args.target,
                          Path(manifest['executable']).name, manifest['version'])
            catalog = package_addons(directory, ROOT / 'dist', args.tag, args.target, ROOT, [args.addon])
            subprocess.run(['cargo', 'run', '--quiet', '--locked', '--manifest-path',
                            str(ROOT / 'crates/addon-sdk/Cargo.toml'), '--example', 'validate-catalog',
                            '--target-dir', str(ROOT / 'target'), '--target', args.target],
                           input=json.dumps(catalog), text=True, check=True)
        elif args.command == 'publish':
            directory = ROOT / 'dist'
            finalize(args.addon, args.tag, directory)
            gh('release', 'upload', args.tag, *(str(p) for p in sorted(directory.iterdir())), '--clobber')
            gh('release', 'edit', args.tag, '--draft=false', '--latest=false')


if __name__ == '__main__':
    main()
