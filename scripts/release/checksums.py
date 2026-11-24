#!/usr/bin/env python3
"""Validate editor-only release assets and generate checksums."""
from pathlib import Path
import sys
from package import RELEASE_TARGETS, asset_name
from addon_release import digest


def generate(directory, tag, root=Path('.')):
    expected = {asset_name(tag, target) for target in RELEASE_TARGETS}
    if {p.name for p in directory.iterdir()} - {'SHA256SUMS'} != expected:
        raise RuntimeError('Unexpected or missing editor release assets')
    for name in expected:
        path = directory / name
        if not path.is_file() or path.is_symlink() or path.stat().st_size == 0:
            raise RuntimeError(f'Invalid release asset: {name}')
    (directory / 'SHA256SUMS').write_text(''.join(
        f'{digest(directory / name)}  {name}\n' for name in sorted(expected)))


if __name__ == '__main__':
    generate(Path(sys.argv[1]), sys.argv[2])
