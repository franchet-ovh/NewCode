#!/usr/bin/env python3
"""Validate committed versions, reserve, package and publish an addon."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tomllib
from addon_release import manifests, package_addons, RELEASE_TARGETS, index_name, digest, archive_name, stable, next_version
from addon_index import load, record, check_metadata, validate_source
from github_api import gh, api

ROOT = Path(__file__).resolve().parents[2]


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


def definition(addon):
    for item in manifests(ROOT):
        if item[1]['id'] == addon:
            return item
    raise ValueError(f'Unknown addon: {addon}')


def check(addon, tag=None):
    path, manifest = definition(addon)
    check_metadata(path, manifest)
    item = record(load(ROOT), addon, manifest['version'])
    if item['status'] not in ('planned', 'published') or any(
            item[key] != manifest[key] for key in ('version', 'protocol', 'editor_version_req')):
        raise ValueError('Addon is not a matching committed release plan')
    if tag is not None and tag != item['release']:
        raise ValueError('Tag does not match committed addon version')
    return manifest, item


def tag_sha(repo, tag):
    ref = api(repo, f'git/ref/tags/{tag}', missing=True)
    if ref is None:
        return None
    obj = ref['object']
    for _ in range(5):
        if obj['type'] == 'commit' and re.fullmatch(r'[0-9a-f]{40}', obj['sha']):
            return obj['sha']
        if obj['type'] != 'tag':
            break
        obj = api(repo, f"git/tags/{obj['sha']}")['object']
    raise ValueError('Addon tag must resolve to a commit')


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


def reserve(addon, sha):
    _, item = check(addon)
    repo = os.environ['GH_REPO']
    tag = item['release']
    content = f'Addon inputs: {fingerprint(addon, sha)}'
    release = api(repo, f'releases/tags/{tag}', missing=True)
    existing_sha = tag_sha(repo, tag)
    if release is not None:
        if existing_sha is None or content not in (release.get('body') or '').splitlines():
            raise ValueError(f'{tag} already exists with different inputs; rerun its original workflow or prepare a newer version')
        subprocess.run(['git', 'merge-base', '--is-ancestor', existing_sha, 'origin/main'], cwd=ROOT, check=True)
        if fingerprint(addon, existing_sha) != fingerprint(addon, sha):
            raise ValueError('Reserved addon tag inputs differ from the requested source')
        return tag, not release['draft'], existing_sha
    if item['status'] != 'planned':
        raise ValueError('Published index version has no corresponding GitHub release')
    if existing_sha is not None and existing_sha != sha:
        raise ValueError('Existing addon tag points to another source commit')
    body = f'Addon source: {sha}\n{content}\n\nIndependent addon release. See addons.json for supported editor versions.\n'
    gh('release', 'create', tag, '--repo', repo, '--target', sha, '--draft', '--title', tag,
       '--notes', body, '--latest=false')
    return tag, False, sha


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
    parser.add_argument('command', choices=['reserve', 'check', 'package', 'publish'])
    parser.add_argument('--addon', required=True)
    parser.add_argument('--tag')
    parser.add_argument('--source-sha')
    parser.add_argument('--target', choices=RELEASE_TARGETS)
    args = parser.parse_args()
    if args.command == 'reserve':
        if not re.fullmatch(r'[0-9a-f]{40}', args.source_sha or ''):
            raise ValueError('Expected a full source commit SHA')
        if subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip() != args.source_sha:
            raise ValueError('Source SHA must match the checked-out commit')
        subprocess.run(['git', 'merge-base', '--is-ancestor', args.source_sha, 'origin/main'], cwd=ROOT, check=True)
        validate_source(ROOT)
        tag, done, sha = reserve(args.addon, args.source_sha)
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            output.write(f'tag={tag}\ndone={str(done).lower()}\nsha={sha}\n')
        return
    if not args.tag:
        parser.error('--tag is required')
    manifest, _ = check(args.addon, args.tag)
    if args.command == 'package':
        if not args.target:
            parser.error('package requires --target')
        from package import verify_binary
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
        gh('release', 'upload', args.tag, '--repo', os.environ['GH_REPO'],
           *(str(p) for p in sorted(directory.iterdir())), '--clobber')
        gh('release', 'edit', args.tag, '--repo', os.environ['GH_REPO'], '--draft=false', '--latest=false')


if __name__ == '__main__':
    main()
