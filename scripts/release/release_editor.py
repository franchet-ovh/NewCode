#!/usr/bin/env python3
"""Release the editor from main, optionally releasing all addons with minor bumps."""
import argparse
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tomllib

from publish_addon import next_version, release_pages, stable
from release_addon import DEFAULT_REPO, bump_version, gh


WORKFLOW = 'release.yml'
ADDON_SCRIPT = Path(__file__).resolve().with_name('release_addon.py')


def validate_version(version):
    if not isinstance(version, str):
        raise ValueError('Editor releases require a stable x.y.z version')
    try:
        stable(version)
    except ValueError as error:
        raise ValueError(f'Editor releases require a stable x.y.z version: {version!r}') from error


def source_version(repo):
    sha = gh('api', f'repos/{repo}/commits/main', '--jq', '.sha')
    if not re.fullmatch(r'[0-9a-f]{40}', sha):
        raise ValueError('GitHub returned an invalid main commit SHA')
    manifest = tomllib.loads(gh('api', f'repos/{repo}/contents/Cargo.toml?ref={sha}',
                               '-H', 'Accept: application/vnd.github.raw+json'))
    if manifest['package']['name'] != 'focus-editor':
        raise ValueError('The main Cargo package must be focus-editor')
    version = manifest['package']['version']
    validate_version(version)
    return sha, version


def release_versions(repo):
    """Editor tags may have a v prefix; addon tags are a separate version stream."""
    versions = set()
    for page in release_pages(repo, query=gh):
        for release in page:
            version = release['tag_name'].removeprefix('v')
            try:
                stable(version)
            except ValueError:
                continue
            # Draft releases reserve their versions too.
            versions.add(version)
    return sorted(versions, key=stable)


def release_addons(repo, editor_url):
    command = [sys.executable, str(ADDON_SCRIPT), '--all', '--minor', '--repo', repo]
    print('\nReleasing all addons with independent minor version bumps…', flush=True)
    try:
        result = subprocess.run(command, check=False)
    except OSError as error:
        raise RuntimeError(f'Editor release was created: {editor_url}\n'
                           f'Could not start the addon release helper: {error}') from error
    if result.returncode:
        raise RuntimeError(f'Editor release was created: {editor_url}\n'
                           f'Addon release helper exited with status {result.returncode}. '
                           'Some addons may already have been dispatched; inspect its output before retrying. '
                           'Retry remaining addons with release_addon.py rather than recreating the editor release.')


def run(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    version = parser.add_mutually_exclusive_group()
    version.add_argument('--version', help='Explicit stable x.y.z editor version; defaults to the current main version')
    for bump in ('major', 'minor', 'patch'):
        version.add_argument(f'--{bump}', action='store_const', dest='bump', const=bump,
                             help=f'Apply a SemVer {bump} bump using the highest source or released/reserved version')
    parser.add_argument('--with-addon', action='store_true',
                        help='After creating the editor release, call release_addon.py --all --minor')
    parser.add_argument('--repo', default=DEFAULT_REPO, help=f'GitHub owner/repo (default: {DEFAULT_REPO})')
    args = parser.parse_args(argv)
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', args.repo):
        parser.error('--repo must be owner/repo')
    if args.version:
        validate_version(args.version)
    if args.with_addon and not ADDON_SCRIPT.is_file():
        raise ValueError(f'Addon release helper not found: {ADDON_SCRIPT}')
    if not shutil.which('gh'):
        raise RuntimeError('GitHub CLI (gh) is required; install it and run gh auth login')
    gh('auth', 'status')
    sha, current = source_version(args.repo)
    versions = release_versions(args.repo)
    if args.bump:
        chosen = bump_version(current, versions, args.bump)
    else:
        try:
            chosen = next_version(current, versions, args.version or current)
        except ValueError as error:
            raise ValueError(f'{error}. Select a newer version with --version, --patch, --minor, or --major.') from error
    tag = f'v{chosen}'
    url = f'https://github.com/{args.repo}/releases/tag/{tag}'
    workflow_url = f'https://github.com/{args.repo}/actions/workflows/{WORKFLOW}'
    print(f'\nRepository: {args.repo}\nSource: main at {sha}\nVersion: {chosen}\nTag: {tag}\n', flush=True)
    try:
        gh('release', 'create', tag, '--repo', args.repo, '--target', sha, '--title', tag, '--generate-notes')
    except (RuntimeError, OSError) as error:
        raise RuntimeError(f'Could not confirm editor release creation: {error}\n'
                           f'Check {url} before retrying. No addon releases were attempted.') from error
    print(f'Editor release created: {url}\n'
          f'The release workflow will commit the version to main and build/upload the downloads; publishing has not finished.\n'
          f'{workflow_url}', flush=True)
    if args.with_addon:
        release_addons(args.repo, url)


def main(argv=None):
    try:
        run(argv)
    except KeyboardInterrupt:
        print('\nRelease cancelled.', file=sys.stderr)
        return 130
    except (RuntimeError, ValueError, KeyError, TypeError, OSError) as error:
        print(f'Error: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
