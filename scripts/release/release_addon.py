#!/usr/bin/env python3
"""Prepare a branch and pull request for selected addon releases from main."""
import argparse
import json
import re
import shutil
import sys
import tomllib
import base64
import uuid

from addon_release import next_version, stable, editor_compatibility, version_files
from addon_index import remote, entry, planned, encode, validate
from github_api import gh, api

DEFAULT_REPO = 'franchet-ovh/NewCode'


class Cancelled(Exception):
    pass


def discover(repo):
    """Read the registry at a single immutable main revision."""
    sha = gh('api', f'repos/{repo}/commits/main', '--jq', '.sha')
    if not re.fullmatch(r'[0-9a-f]{40}', sha):
        raise ValueError('GitHub returned an invalid main commit SHA')
    index, _ = remote(repo, sha)
    return sha, index


def contents(repo, sha, path):
    return gh('api', f'repos/{repo}/contents/{path}?ref={sha}',
              '-H', 'Accept: application/vnd.github.raw+json')


def create_pr(repo, sha, changes, releases):
    # A single tree/commit publishes the complete batch to the new branch.
    tree = []
    for path, content in sorted(changes.items()):
        blob = api(repo, 'git/blobs', {'content': base64.b64encode(content.encode()).decode(), 'encoding': 'base64'})
        tree.append({'path': path, 'mode': '100644', 'type': 'blob', 'sha': blob['sha']})
    base = api(repo, f'git/commits/{sha}')
    tree = api(repo, 'git/trees', {'base_tree': base['tree']['sha'], 'tree': tree})
    title = 'chore: release addons ' + ', '.join(f'{addon} {version}' for addon, version in releases)
    commit = api(repo, 'git/commits', {'message': title, 'tree': tree['sha'], 'parents': [sha]})
    branch = f'release/addons-{sha[:12]}-{uuid.uuid4().hex[:12]}'
    api(repo, 'git/refs', {'ref': f'refs/heads/{branch}', 'sha': commit['sha']})
    url = f'https://github.com/{repo}/tree/{branch}'
    body = ('Prepare independently versioned addon releases:\n\n' +
            '\n'.join(f'- `{addon}`: `{version}`' for addon, version in releases) +
            '\n\nAfter merging, main CI builds the committed versions. The addon index exposes downloads only after successful publication.')
    try:
        pull = api(repo, 'pulls', {'title': title, 'head': branch, 'base': 'main', 'body': body})
    except (RuntimeError, OSError) as error:
        raise RuntimeError(f'Branch created: {url}\nPR creation failed: {error}\n'
                           f'Create a PR from {branch} to main; do not rerun the helper for this batch.') from error
    print(f"Release PR created: {pull['html_url']}\nMerge after review and CI to publish the addons.")


def bump_version(current, versions, bump):
    major, minor, patch = max(stable(v) for v in [current, *versions])
    version = {
        'patch': f'{major}.{minor}.{patch + 1}',
        'minor': f'{major}.{minor + 1}.0',
        'major': f'{major + 1}.0.0',
    }[bump]
    return next_version(current, versions, version)


def version_choices(current, versions):
    choices = []
    for bump, label in [
        ('patch', 'Patch — backward-compatible fixes'),
        ('minor', 'Minor — backward-compatible features'),
        ('major', 'Major — incompatible changes'),
    ]:
        try:
            version = bump_version(current, versions, bump)
        except ValueError:
            continue
        choices.append((version, f'{version}  ({label})'))
    return choices


def choose(title, choices, default=None):
    print(f'\n{title}')
    for index, (_, label) in enumerate(choices, 1):
        suffix = ' [default]' if index == default else ''
        print(f'  {index}) {label}{suffix}')
    print('  0) Cancel')
    while True:
        answer = input('Choice: ').strip()
        if answer.lower() in ('0', 'q', 'quit', 'cancel'):
            raise Cancelled()
        if not answer and default is not None:
            return choices[default - 1][0]
        if answer.isascii() and answer.isdigit() and 1 <= int(answer) <= len(choices):
            return choices[int(answer) - 1][0]
        print(f'Enter a number from 1 to {len(choices)}, or 0 to cancel.')


def select_version(current, versions):
    print(f'\nSource version: {current}')
    print(f'Highest released/reserved version: {versions[-1] if versions else "none"}')
    choices = version_choices(current, versions)
    version = choose('Select the next stable SemVer version:',
                     [*choices, (None, 'Custom stable version')], default=1)
    if version is not None:
        return version
    while True:
        version = input('Version (x.y.z; q to cancel): ').strip()
        if version.lower() in ('q', 'quit', 'cancel'):
            raise Cancelled()
        if not version:
            print('Enter a stable x.y.z version')
            continue
        try:
            return next_version(current, versions, version)
        except ValueError as error:
            print(error)


def run(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    selection = parser.add_mutually_exclusive_group()
    selection.add_argument('--addon', action='append', help='Addon ID; repeat to select several; prompt if omitted')
    selection.add_argument('--all', action='store_true', help='Select all addons discovered on main')
    version = parser.add_mutually_exclusive_group()
    version.add_argument('--version', help='Explicit stable x.y.z version for every selected addon')
    for bump in ('major', 'minor', 'patch'):
        version.add_argument(f'--{bump}', action='store_const', dest='bump', const=bump,
                             help=f'Automatically apply a SemVer {bump} bump to each selected addon')
    parser.add_argument('--repo', default=DEFAULT_REPO, help=f'GitHub owner/repo (default: {DEFAULT_REPO})')
    parser.add_argument('--from', dest='editor_from', default='', help='Set inclusive minimum supported editor version (x.y.z)')
    parser.add_argument('--to', dest='editor_to', default='', help='Set exclusive maximum supported editor version (x.y.z)')
    args = parser.parse_args(argv)
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', args.repo):
        parser.error('--repo must be owner/repo')
    if not sys.stdin.isatty() and (not (args.addon or args.all) or not (args.version or args.bump)):
        parser.error('Noninteractive use requires --addon or --all, and --version, --major, --minor, or --patch')
    if args.version:
        stable(args.version)
    for bound in (args.editor_from, args.editor_to):
        if bound:
            stable(bound)
    if not shutil.which('gh'):
        raise RuntimeError('GitHub CLI (gh) is required; install it and run gh auth login')
    gh('auth', 'status')
    sha, index = discover(args.repo)
    # Historical addons can remain in the index after their source is removed.
    tree = api(args.repo, f'git/trees/{sha}?recursive=1')
    if tree.get('truncated'):
        raise ValueError('GitHub truncated the source tree')
    paths = {item['path'] for item in tree['tree'] if item['type'] == 'blob'}
    addons = {a['id']: a['path'] for a in index['addons'] if a['path'] + '/addon.toml' in paths}
    if not addons:
        raise ValueError('No indexed source addons found on main')
    if args.all:
        selected = sorted(addons)
    elif args.addon:
        selected = list(dict.fromkeys(args.addon))
    else:
        selected = [choose('Select an addon:', [(a, a) for a in sorted(addons)])]
    for addon in selected:
        if addon not in addons:
            raise ValueError(f'Unknown addon: {addon}. Available: {", ".join(sorted(addons))}')
    # Validate every selected manifest/version before creating any remote objects.
    releases = []
    changes = {}
    for addon in selected:
        path = addons[addon]
        files = {name: contents(args.repo, sha, f'{path}/{name}')
                 for name in ('addon.toml', 'Cargo.toml', 'Cargo.lock')}
        manifest = tomllib.loads(files['addon.toml'])
        if manifest['id'] != addon:
            raise ValueError('Addon manifest ID does not match its directory')
        current = manifest['version']
        stable(current)
        editor_compatibility(manifest, args.editor_from, args.editor_to)
        versions = sorted((v['version'] for v in entry(index, addon)['versions']), key=stable)
        if args.version:
            version = next_version(current, [current, *versions], args.version)
        elif args.bump:
            version = bump_version(current, versions, args.bump)
        else:
            print(f'\nAddon: {addon}')
            version = select_version(current, [current, *versions])
        prepared = version_files(files['addon.toml'], files['Cargo.toml'], files['Cargo.lock'],
                                 version, args.editor_from, args.editor_to)
        changes.update({f'{path}/{name}': text for name, text in prepared.items()})
        manifest = tomllib.loads(prepared['addon.toml'])
        for old in entry(index, addon)['versions']:
            if old['status'] == 'planned':
                old['status'] = 'reserved'
        entry(index, addon)['versions'].append(planned(manifest))
        releases.append((addon, version))
    validate(index)
    changes['addons.json'] = encode(index)
    print(f'\nRepository: {args.repo}\nSource: main at {sha}')
    for addon, version in releases:
        print(f'Addon: {addon}  Version: {version}  Tag: addon_{addon}_{version}')
    create_pr(args.repo, sha, changes, releases)


def main(argv=None):
    try:
        run(argv)
    except (Cancelled, EOFError, KeyboardInterrupt):
        print('\nRelease cancelled.', file=sys.stderr)
        return 130
    except (RuntimeError, ValueError, KeyError, TypeError, StopIteration, OSError) as error:
        print(f'Error: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
