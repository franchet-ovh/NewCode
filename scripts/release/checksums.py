#!/usr/bin/env python3
"""Validate editor-only release assets and generate checksums."""
import argparse
from pathlib import Path
from package import MACOS_TARGETS, RELEASE_TARGETS, asset_name
from addon_release import digest


def generate(directory, tag, root=Path('.'), platform="linux"):
    targets, manifest = {
        "linux": (RELEASE_TARGETS, "SHA256SUMS"),
        "macos": (MACOS_TARGETS, "SHA256SUMS-macos"),
    }[platform]
    expected = {asset_name(tag, target) for target in targets}
    if {p.name for p in directory.iterdir()} - {manifest} != expected:
        raise RuntimeError('Unexpected or missing editor release assets')
    for name in expected:
        path = directory / name
        if not path.is_file() or path.is_symlink() or path.stat().st_size == 0:
            raise RuntimeError(f'Invalid release asset: {name}')
    (directory / manifest).write_text(''.join(
        f'{digest(directory / name)}  {name}\n' for name in sorted(expected)))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("tag")
    parser.add_argument("--platform", choices=("linux", "macos"), default="linux")
    args = parser.parse_args()
    generate(args.directory, args.tag, platform=args.platform)
