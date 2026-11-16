"""Stamp release checkout manifests and lockfiles without resolving dependencies."""
import argparse
from pathlib import Path
import re
import tomllib

from addon_release import manifests


def release_version(tag):
    version = tag.removeprefix("v")
    number = r"(?:0|[1-9][0-9]*)"
    identifier = r"[0-9A-Za-z-]+"
    if not re.fullmatch(rf"{number}\.{number}\.{number}(?:-{identifier}(?:\.{identifier})*)?(?:\+{identifier}(?:\.{identifier})*)?", version):
        raise ValueError(f"Release tag must be a semantic version with optional v prefix: {tag!r}")
    prerelease = version.split("+", 1)[0].partition("-")[2]
    if any(part.isdigit() and len(part) > 1 and part.startswith("0") for part in prerelease.split(".")):
        raise ValueError("Numeric prerelease identifiers must not have leading zeros")
    if any(int(part) > 2**64 - 1 for part in version.split("-", 1)[0].split("+", 1)[0].split(".")):
        raise ValueError("Release version components exceed Cargo's supported range")
    return version


def replace_version(text, version):
    text, count = re.subn(r'(?m)^version\s*=\s*["\'][^"\']*["\']', f'version = "{version}"', text)
    if count != 1:
        raise ValueError("Expected exactly one version in TOML section")
    return text


def validate_versions(tag, root=Path(".")):
    version = release_version(tag)
    projects = [root / "Cargo.toml"]
    for path, manifest in manifests(root):
        if manifest["version"] != version:
            raise ValueError(f"Addon version does not match release: {path}")
        projects.append(path.with_name("Cargo.toml"))
    for path in projects:
        if tomllib.loads(path.read_text())["package"]["version"] != version:
            raise ValueError(f"Cargo version does not match release: {path}")
    return version


def prepare(tag, root=Path(".")):
    version = release_version(tag)
    changes = {}
    projects = [root / "Cargo.toml"]
    for path, _ in manifests(root):
        changes[path] = replace_version(path.read_text(), version)
        projects.append(path.with_name("Cargo.toml"))
    for project in projects:
        text = project.read_text()
        name = tomllib.loads(text)["package"]["name"]
        sections = re.split(r'(?m)(?=^\[)', text)
        for index, section in enumerate(sections):
            if section.startswith("[package]\n"):
                sections[index] = replace_version(section, version)
                break
        else:
            raise ValueError(f"Missing package section: {project}")
        changes[project] = "".join(sections)
        lock = project.with_name("Cargo.lock")
        blocks = lock.read_text().split("[[package]]")
        matches = 0
        for index in range(1, len(blocks)):
            package = tomllib.loads(blocks[index])
            if package["name"] == name and "source" not in package:
                blocks[index] = replace_version(blocks[index], version)
                matches += 1
        if matches != 1:
            raise ValueError(f"Expected one local package {name} in {lock}")
        changes[lock] = "[[package]]".join(blocks)
    # Validate every input before writing any changes.
    for path, text in changes.items():
        tomllib.loads(text)
    for path, text in changes.items():
        path.write_text(text)
    return version


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    args = parser.parse_args()
    print(f"Prepared release version {prepare(args.tag)}")
