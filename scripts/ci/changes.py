#!/usr/bin/env python3
"""Select Cargo consumers of changed paths; unknown inputs fail closed."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[2]


def projects(root):
    result = {'editor': root / 'Cargo.toml'}
    result.update({p.parent.name: p for p in (root / 'crates').glob('*/Cargo.toml')})
    result.update({p.parent.name: p for p in (root / 'addons').glob('*/Cargo.toml')})
    return result


def local_dependencies(manifest):
    data = tomllib.loads(manifest.read_text())
    tables = [data.get(name, {}) for name in ('dependencies', 'build-dependencies', 'dev-dependencies')]
    for target in data.get('target', {}).values():
        tables += [target.get(name, {}) for name in ('dependencies', 'build-dependencies', 'dev-dependencies')]
    return {(manifest.parent / dep['path'] / 'Cargo.toml').resolve()
            for table in tables for dep in table.values() if isinstance(dep, dict) and 'path' in dep}


def select(paths, root=ROOT):
    manifests = projects(root)
    affected = set()
    tooling = False
    for path in paths:
        p = Path(path)
        if path == 'addons.json':
            tooling = True
            continue
        if p.suffix == '.md' or path.startswith('ui-mocks/') or p.suffix == '.png' and not path.startswith('addons/'):
            continue
        if path.startswith('scripts/release/'):
            tooling = True
            # Publisher changes must exercise addon builds as well.
            affected.update(manifests)
            continue
        owners = [key for key, manifest in manifests.items()
                  if manifest.parent != root and p.is_relative_to(manifest.parent.relative_to(root))]
        if owners:
            affected.update(owners)
        elif path.startswith(('src/', 'tests/')) or path in ('Cargo.toml', 'Cargo.lock'):
            affected.add('editor')
        else:
            affected.update(manifests)
            tooling = True
    while True:
        previous = affected.copy()
        for key, manifest in manifests.items():
            for dependency in local_dependencies(manifest):
                if any(other in affected and other_path.resolve() == dependency for other, other_path in manifests.items()):
                    affected.add(key)
        if previous == affected:
            break
    return sorted(affected), tooling


def changed_paths(base, head='HEAD', merge_base=False, root=ROOT):
    if not base.strip('0'):
        raise ValueError('New branch')
    if merge_base:
        base = subprocess.check_output(['git', 'merge-base', base, head], cwd=root, text=True).strip()
    else:
        subprocess.run(['git', 'merge-base', '--is-ancestor', base, head], cwd=root, check=True)
    return [p for p in subprocess.check_output(
        ['git', 'diff', '--name-only', '--no-renames', '-z', base, head], cwd=root
    ).decode().split('\0') if p]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--base', required=True)
    parser.add_argument('--head', default='HEAD')
    parser.add_argument('--merge-base', action='store_true')
    args = parser.parse_args()
    try:
        affected, tooling = select(changed_paths(args.base, args.head, args.merge_base))
    except (ValueError, subprocess.CalledProcessError):
        affected, tooling = sorted(projects(ROOT)), True
    matrix = []
    for component in affected:
        for runner, target in [('ubuntu-24.04', 'x86_64-unknown-linux-gnu'), ('ubuntu-24.04-arm', 'aarch64-unknown-linux-gnu')]:
            matrix.append(dict(component=component, runner=runner, target=target))
    from sys import path as module_path
    module_path.insert(0, str(ROOT / 'scripts/release'))
    from addon_release import build_matrix
    for component in affected:
        manifest = ROOT / 'addons' / component / 'addon.toml'
        if manifest.is_file():
            definition = tomllib.loads(manifest.read_text())
            if definition.get('execution') == 'workspace':
                matrix.extend(dict(component=component, **item) for item in build_matrix(definition)['include'] if item['target'].endswith('-musl'))
    result = {'matrix': {'include': matrix}, 'has_checks': bool(matrix), 'tooling': tooling,
              'addons': [a for a in affected if (ROOT / 'addons' / a / 'addon.toml').is_file()]}
    if os.environ.get('GITHUB_OUTPUT'):
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            for key, value in result.items():
                output.write(f'{key}={json.dumps(value)}\n')
    print(json.dumps(result, indent=2))
    if os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(os.environ['GITHUB_STEP_SUMMARY'], 'a') as output:
            output.write('Selected checks: ' + ', '.join(affected) + ('; Python tooling' if tooling else '') + '\n')


if __name__ == '__main__':
    main()
