#!/usr/bin/env python3
"""Release selected addons with interactive or automatic SemVer bumps on main."""
import argparse
import json
import re
import shutil
import subprocess
import sys
import tomllib

from publish_addon import next_version, stable, release_pages as github_release_pages
from addon_release import require_editor_compatibility

DEFAULT_REPO = 'franchet-ovh/NewCode'
WORKFLOW = 'addon-release.yml'


class Cancelled(Exception):
    pass


def gh(*args):
    result = subprocess.run(['gh', *args], text=True, capture_output=True)
    if result.returncode:
        message = result.stderr.strip() or result.stdout.strip() or f'exit code {result.returncode}'
        raise RuntimeError(f'GitHub CLI failed: {message}')
    return result.stdout.strip()


def discover(repo):
    """Use one immutable main revision for discovery and the eventual build."""
    sha = gh('api', f'repos/{repo}/commits/main', '--jq', '.sha')
    if not re.fullmatch(r'[0-9a-f]{40}', sha):
        raise ValueError('GitHub returned an invalid main commit SHA')
    tree = json.loads(gh('api', f'repos/{repo}/git/trees/{sha}?recursive=1'))
    if tree.get('truncated'):
        raise ValueError('GitHub truncated the repository tree; cannot discover all addons')
    addons = {}
    for entry in tree['tree']:
        match = re.fullmatch(r'addons/([A-Za-z0-9_-]+)/addon\.toml', entry['path'])
        if match and entry['type'] == 'blob':
            addons[match[1]] = entry['path']
    if not addons:
        raise ValueError(f'No addon manifests found on {repo} main')
    return sha, addons


def source_version(repo, sha, addon, path):
    manifest = tomllib.loads(gh('api', f'repos/{repo}/contents/{path}?ref={sha}',
                               '-H', 'Accept: application/vnd.github.raw+json'))
    if manifest['id'] != addon:
        raise ValueError('Addon manifest ID does not match its directory')
    require_editor_compatibility(manifest)
    stable(manifest['version'])
    return manifest['version']


def dispatch_inputs(workflow):
    """Read input types/choices from the workflow's block-style YAML mapping.

    Keep workflow_call inputs separate: reusable and manual triggers can declare
    different inputs. No YAML dependency is needed for these mapping keys.
    """
    event_indent = inputs_indent = field_indent = property_indent = options_indent = None
    inputs = {}
    current = None
    for line in workflow.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith('#'):
            continue
        indent = len(line) - len(line.lstrip(' '))
        key = re.fullmatch(r'([A-Za-z0-9_-]+):(?:\s+(.*))?', stripped)
        if event_indent is None:
            if key and key[1] == 'workflow_dispatch':
                event_indent = indent
            continue
        if indent <= event_indent:
            break
        if inputs_indent is None:
            if key and key[1] == 'inputs':
                inputs_indent = indent
            continue
        if indent <= inputs_indent:
            break
        if key:
            if field_indent is None:
                field_indent = indent
            if indent == field_indent:
                if key[2] and not key[2].startswith('#'):
                    raise ValueError('Workflow input declarations must use block-style YAML mappings')
                current = key[1]
                inputs[current] = {}
                property_indent = options_indent = None
                continue
        if current is None:
            continue
        if options_indent is not None:
            if indent >= options_indent and stripped.startswith('- '):
                inputs[current]['options'].append(yaml_name(stripped[2:]))
                continue
            if indent <= options_indent:
                options_indent = None
        if not key:
            continue
        if property_indent is None:
            property_indent = indent
        if indent != property_indent:
            continue
        if key[1] == 'type':
            inputs[current]['type'] = yaml_name(key[2] or '')
        elif key[1] == 'options':
            value = key[2] or ''
            inputs[current]['options'] = []
            if not value or value.startswith('#'):
                options_indent = indent
            else:
                inline = re.fullmatch(r'\[(.*)\]\s*(?:#.*)?', value)
                if not inline:
                    raise ValueError('Workflow choices must be a list of plain or quoted names')
                if inline[1].strip():
                    inputs[current]['options'] = [yaml_name(item.strip()) for item in inline[1].split(',') if item.strip()]
    return inputs


def yaml_name(value):
    """Types and addon/version choices are simple names, optionally quoted."""
    match = re.fullmatch(r'''(?:"([A-Za-z0-9_.-]+)"|'([A-Za-z0-9_.-]+)'|([A-Za-z0-9_.-]+))\s*(?:#.*)?''', value.strip())
    if not match:
        raise ValueError(f'Unsupported workflow type or choice value: {value!r}')
    return next(group for group in match.groups() if group is not None)


def validate_choices(inputs, name, values):
    definition = inputs[name]
    kind = definition.get('type', 'string')
    if kind == 'string':
        return
    if kind != 'choice' or not definition.get('options'):
        raise ValueError(f'Unsupported manual workflow input definition for {name}')
    allowed = definition['options']
    rejected = sorted(set(values) - set(allowed))
    if rejected:
        raise ValueError(f'The release workflow on main does not allow {name} value(s): {", ".join(rejected)}. '
                         f'Allowed: {", ".join(allowed)}. '
                         f'Merge the .github/workflows/{WORKFLOW} change ({name} type: string) into main '
                         'before releasing these addons. No workflows were dispatched.')


def workflow_inputs(repo, sha):
    workflow = gh('api', f'repos/{repo}/contents/.github/workflows/{WORKFLOW}?ref={sha}',
                  '-H', 'Accept: application/vnd.github.raw+json')
    inputs = dispatch_inputs(workflow)
    missing = {'addon', 'version'} - inputs.keys()
    if missing:
        raise ValueError(f'The workflow on main does not declare required manual inputs: {", ".join(sorted(missing))}')
    return inputs


def release_pages(repo):
    return github_release_pages(repo, query=gh)


def release_versions(repo, addon, pages=None):
    if pages is None:
        pages = release_pages(repo)
    prefix = f'addon_{addon}_'
    versions = set()
    for page in pages:
        for release in page:
            tag = release['tag_name']
            if tag.startswith(prefix):
                version = tag[len(prefix):]
                try:
                    stable(version)
                except ValueError:
                    continue
                # Draft versions are reserved too; never suggest reusing one.
                versions.add(version)
    return sorted(versions, key=stable)


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
    args = parser.parse_args(argv)
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', args.repo):
        parser.error('--repo must be owner/repo')
    if not sys.stdin.isatty() and (not (args.addon or args.all) or not (args.version or args.bump)):
        parser.error('Noninteractive use requires --addon or --all, and --version, --major, --minor, or --patch')
    if args.version:
        stable(args.version)
    if not shutil.which('gh'):
        raise RuntimeError('GitHub CLI (gh) is required; install it and run gh auth login')
    gh('auth', 'status')
    sha, addons = discover(args.repo)
    if args.all:
        selected = sorted(addons)
    elif args.addon:
        selected = list(dict.fromkeys(args.addon))
    else:
        selected = [choose('Select an addon:', [(a, a) for a in sorted(addons)])]
    for addon in selected:
        if addon not in addons:
            raise ValueError(f'Unknown addon: {addon}. Available: {", ".join(sorted(addons))}')
    # Resolve every version before starting remote workflows. A bad manifest,
    # invalid version, or cancelled prompt must not dispatch a partial batch.
    pages = None
    releases = []
    for addon in selected:
        current = source_version(args.repo, sha, addon, addons[addon])
        if pages is None:
            pages = release_pages(args.repo)
        versions = release_versions(args.repo, addon, pages)
        if args.version:
            version = next_version(current, versions, args.version)
        elif args.bump:
            version = bump_version(current, versions, args.bump)
        else:
            print(f'\nAddon: {addon}')
            version = select_version(current, versions)
        releases.append((addon, version))
    inputs = workflow_inputs(args.repo, sha)
    pin_source = 'source-sha' in inputs
    validate_choices(inputs, 'addon', selected)
    validate_choices(inputs, 'version', [version for _, version in releases])
    if pin_source:
        validate_choices(inputs, 'source-sha', [sha])
    print(f'\nRepository: {args.repo}\nSource: main at {sha}')
    if not pin_source:
        print('The workflow does not accept source-sha; releases will build main at dispatch time.')
    for addon, version in releases:
        print(f'\nAddon: {addon}\nVersion: {version}\nTag: addon_{addon}_{version}')
    print(flush=True)
    url = f'https://github.com/{args.repo}/actions/workflows/{WORKFLOW}'
    dispatched = []
    for index, (addon, version) in enumerate(releases):
        try:
            command = ['workflow', 'run', WORKFLOW, '--repo', args.repo, '--ref', 'main',
                       '-f', f'addon={addon}', '-f', f'version={version}']
            if pin_source:
                command += ['-f', f'source-sha={sha}']
            gh(*command)
        except (RuntimeError, OSError) as error:
            remaining = ', '.join(a for a, _ in releases[index + 1:]) or 'none'
            raise RuntimeError(f'Dispatch failed for {addon}: {error}\n'
                               f'Already dispatched: {", ".join(dispatched) or "none"}\n'
                               f'Not attempted: {remaining}\n'
                               f'Check the status of {addon} before retrying: {url}') from error
        dispatched.append(addon)
        print(f'Release workflow dispatched for {addon}; publishing has not finished.', flush=True)
    print(url)


def main(argv=None):
    try:
        run(argv)
    except (Cancelled, EOFError, KeyboardInterrupt):
        print('\nRelease cancelled.', file=sys.stderr)
        return 130
    except (RuntimeError, ValueError, KeyError, TypeError, OSError) as error:
        print(f'Error: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
