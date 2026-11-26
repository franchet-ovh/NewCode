"""Repository addon registry: release intent, history, and verified download metadata."""
import argparse
import base64
import copy
import json
import os
from pathlib import Path
import re
import subprocess
import time
import tomllib

from addon_release import manifests, stable, require_editor_compatibility, RELEASE_TARGETS
from github_api import api, gh, GitHubError

ROOT = Path(__file__).resolve().parents[2]
STATES = {'planned', 'published', 'reserved'}


def encode(index):
    validate(index)
    result = copy.deepcopy(index)
    result['addons'].sort(key=lambda addon: addon['id'])
    for addon in result['addons']:
        addon['versions'].sort(key=lambda version: stable(version['version']), reverse=True)
        for version in addon['versions']:
            version['packages'].sort(key=lambda package: package['target'])
    return json.dumps(result, indent=2) + '\n'


def validate(index):
    if index.get('schema') != 1 or not isinstance(index.get('addons'), list):
        raise ValueError('Unsupported addon index schema')
    ids = set()
    for addon in index['addons']:
        addon_id = addon['id']
        if not re.fullmatch(r'[A-Za-z0-9_-]+', addon_id) or addon_id in ids or addon['path'] != f'addons/{addon_id}':
            raise ValueError('Invalid or duplicate addon index identity/path')
        ids.add(addon_id)
        versions = set()
        for version in addon['versions']:
            stable(version['version'])
            if version['version'] in versions or version['release'] != f"addon_{addon_id}_{version['version']}" or version['status'] not in STATES:
                raise ValueError('Invalid or duplicate addon index version')
            versions.add(version['version'])
            if version['status'] == 'published':
                validate_packages(addon_id, version)
            elif version['packages']:
                raise ValueError('Unpublished addon versions must not advertise packages')
            if version['status'] == 'planned':
                require_editor_compatibility(dict(version, id=addon_id))
                if type(version['protocol']) is not int or version['protocol'] < 1:
                    raise ValueError('Invalid addon protocol')
    return index


def validate_packages(addon_id, version):
    packages = version['packages']
    if not packages:
        raise ValueError('Published addon version has no verified packages')
    targets = set()
    for package in packages:
        if any(package[key] != expected for key, expected in (
                ('id', addon_id), ('version', version['version']),
                ('protocol', version['protocol']), ('editor_version_req', version['editor_version_req']))):
            raise ValueError('Addon index and package identity/compatibility differ')
        require_editor_compatibility(package)
        if (not isinstance(package['name'], str) or not package['name'] or
                type(package['protocol']) is not int or package['protocol'] < 1 or
                not isinstance(package['target'], str) or not package['target'] or package['target'] in targets or
                not re.fullmatch(r'[^./\\][^/\\]*\.tar\.gz', package['archive']) or
                type(package['size']) is not int or not 0 < package['size'] <= 256 * 1024 * 1024 or
                not re.fullmatch(r'[0-9a-fA-F]{64}', package['sha256']) or
                package.get('source_release', version['release']) != version['release']):
            raise ValueError('Invalid addon package metadata')
        targets.add(package['target'])


def entry(index, addon_id):
    for addon in index['addons']:
        if addon['id'] == addon_id:
            return addon
    raise ValueError(f'Unknown indexed addon: {addon_id}')


def record(index, addon_id, version):
    for item in entry(index, addon_id)['versions']:
        if item['version'] == version:
            return item
    raise ValueError(f'Addon {addon_id} version {version} is not indexed; prepare a release PR first')


def planned(manifest):
    return {'version': manifest['version'], 'release': f"addon_{manifest['id']}_{manifest['version']}",
            'status': 'planned', 'protocol': manifest['protocol'],
            'editor_version_req': require_editor_compatibility(manifest), 'packages': []}


def load(root=ROOT):
    return validate(json.loads((root / 'addons.json').read_text()))


def remote(repo, ref='main'):
    file = api(repo, f'contents/addons.json?ref={ref}')
    if file.get('encoding') == 'none':
        # GitHub omits inline content for files larger than 1 MB.
        text = gh('api', f'repos/{repo}/contents/addons.json?ref={ref}',
                  '-H', 'Accept: application/vnd.github.raw+json')
    else:
        text = base64.b64decode(file['content'])
    return validate(json.loads(text)), file['sha']


def validate_source(root=ROOT, index=None):
    index = load(root) if index is None else validate(index)
    definitions = {manifest['id']: (path, manifest) for path, manifest in manifests(root)}
    # Historical records for removed addons remain discoverable; their source
    # directory cannot contain a planned release until it exists again.
    if set(definitions) - {addon['id'] for addon in index['addons']}:
        raise ValueError('Every source addon must have an addons.json entry')
    for path, manifest in definitions.values():
        check_metadata(path, manifest)
    selected = []
    for addon in index['addons']:
        for version in addon['versions']:
            if version['status'] != 'planned':
                continue
            if addon['id'] not in definitions:
                raise ValueError(f"Planned addon has no source: {addon['id']}")
            path, manifest = definitions[addon['id']]
            if any(version[key] != manifest[key] for key in ('version', 'protocol', 'editor_version_req')):
                raise ValueError(f"Planned addon differs from committed manifest: {addon['id']}")
            check_metadata(path, manifest)
            selected.append(addon['id'])
    return sorted(selected)


def check_metadata(path, manifest):
    require_editor_compatibility(manifest)
    stable(manifest['version'])
    project = tomllib.loads(path.with_name('Cargo.toml').read_text())['package']
    packages = tomllib.loads(path.with_name('Cargo.lock').read_text())['package']
    local = [p for p in packages if p['name'] == project['name'] and 'source' not in p]
    if project['version'] != manifest['version'] or len(local) != 1 or local[0]['version'] != manifest['version']:
        raise ValueError('Committed addon manifest, Cargo package, and lockfile versions differ')


def validate_transition(before, after):
    """PRs may append intent; automation may fill the corresponding published record."""
    validate(before)
    validate(after)
    old_ids = {addon['id'] for addon in before['addons']}
    for addon in after['addons']:
        if addon['id'] not in old_ids and any(v['status'] != 'planned' for v in addon['versions']):
            raise ValueError('New addons must start with planned releases')
    for old_addon in before['addons']:
        try:
            new_addon = entry(after, old_addon['id'])
        except ValueError as error:
            raise ValueError('Addon version history cannot be removed') from error
        for old in old_addon['versions']:
            try:
                new = record(after, old_addon['id'], old['version'])
            except ValueError as error:
                raise ValueError('Addon version history cannot be removed') from error
            if old == new:
                continue
            if old['status'] != 'planned' or new['status'] not in ('published', 'reserved') or any(
                    old[key] != new[key] for key in ('version', 'release', 'protocol', 'editor_version_req')):
                raise ValueError('Existing addon version records are immutable except publication or retirement of a plan')
        old_versions = {v['version'] for v in old_addon['versions']}
        for new in new_addon['versions']:
            if new['version'] not in old_versions and (new['status'] != 'planned' or
                    any(stable(new['version']) <= stable(v) for v in old_versions)):
                raise ValueError('New addon releases must be planned above every indexed version')


def register(index, addon_id, tag, catalog):
    """Merge one verified release without overwriting newer plans or other addons."""
    result = copy.deepcopy(index)
    version = tag.removeprefix(f'addon_{addon_id}_')
    item = record(result, addon_id, version)
    if item['release'] != tag or item['status'] not in ('planned', 'published') or catalog['schema'] != 2 or catalog['release'] != tag:
        raise ValueError('Release is not a matching planned/published index entry')
    if {p['target'] for p in catalog['packages']} != set(RELEASE_TARGETS):
        raise ValueError('Both release targets must be published before indexing')
    updated = dict(item, status='published', packages=sorted(catalog['packages'], key=lambda package: package['target']))
    validate_packages(addon_id, updated)
    if item['status'] == 'published' and item != updated:
        raise ValueError('Published index packages cannot be replaced')
    item.update(updated)
    validate(result)
    return result


def update_remote(repo, addon_id, tag):
    release = api(repo, f'releases/tags/{tag}')
    if release['draft'] or release['prerelease']:
        raise ValueError('Cannot index an unpublished release')
    # Read only the known release asset, never the repository release list.
    catalog = json.loads(gh('release', 'download', tag, '--repo', repo, '--pattern', 'addons.json', '--output', '-'))
    for attempt in range(5):
        index, blob_sha = remote(repo)
        updated = register(index, addon_id, tag, catalog)
        if updated == index:
            return
        body = {'message': f'chore: index {tag}', 'branch': 'main', 'sha': blob_sha,
                'content': base64.b64encode(encode(updated).encode()).decode()}
        try:
            gh('api', f'repos/{repo}/contents/addons.json', '--method', 'PUT', '--input', '-', input_data=json.dumps(body))
            return
        except GitHubError as error:
            if error.status != 409 or attempt == 4:
                raise
            time.sleep(attempt + 1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['check', 'select', 'register'])
    parser.add_argument('--base', help='Validate history against this Git revision')
    parser.add_argument('--addon')
    parser.add_argument('--tag')
    args = parser.parse_args()
    if args.command == 'register':
        if not args.addon or not args.tag:
            parser.error('register requires --addon and --tag')
        update_remote(os.environ['GH_REPO'], args.addon, args.tag)
        return
    index = load()
    selected = validate_source(index=index)
    if args.base:
        # Bootstrap can introduce the index once; subsequent revisions preserve history.
        exists = subprocess.run(['git', 'cat-file', '-e', f'{args.base}:addons.json'], cwd=ROOT, capture_output=True)
        if exists.returncode == 0:
            before = json.loads(subprocess.check_output(['git', 'show', f'{args.base}:addons.json'], cwd=ROOT))
            validate_transition(before, index)
    if args.command == 'select':
        if os.environ.get('GITHUB_OUTPUT'):
            with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
                output.write(f'addons={json.dumps(selected)}\n')
        print(json.dumps(selected))
    else:
        print('Addon index and committed release metadata are valid')


if __name__ == '__main__':
    main()
