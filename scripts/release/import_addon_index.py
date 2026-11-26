#!/usr/bin/env python3
"""One-time import of addon release history; writes only the local index file."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import re
import sys

from addon_index import encode, validate_packages
from addon_release import manifests, stable
from github_api import gh
from publish_addon import release_pages


def import_history(repo, root=Path('.'), query=gh):
    addons = {m['id']: {'id': m['id'], 'path': f"addons/{m['id']}", 'versions': []}
              for _, m in manifests(root)}
    releases = []
    for page in release_pages(repo, query=query):
        for release in page:
            match = re.fullmatch(r'addon_([A-Za-z0-9_-]+)_(\d+\.\d+\.\d+)', release['tag_name'])
            if not match:
                continue
            try:
                stable(match[2])
            except ValueError:
                continue
            releases.append((match[1], match[2], release))

    def convert(item):
        addon_id, version, release = item
        record = {'version': version, 'release': release['tag_name'], 'status': 'reserved', 'packages': []}
        warning = None
        if not release['draft'] and not release['prerelease']:
            try:
                catalog = json.loads(query('release', 'download', release['tag_name'], '--repo', repo,
                                           '--pattern', 'addons.json', '--output', '-'))
                if catalog['schema'] != 2 or catalog['release'] != record['release'] or not catalog['packages']:
                    raise ValueError('Invalid or missing independent catalog')
                first = catalog['packages'][0]
                published = dict(record, status='published', packages=catalog['packages'],
                                 protocol=first['protocol'], editor_version_req=first['editor_version_req'])
                validate_packages(addon_id, published)
                assets = {asset['name']: asset for asset in release['assets']}
                for package in published['packages']:
                    if package['archive'] not in assets or assets[package['archive']]['size'] != package['size']:
                        raise ValueError('Catalog references a missing or differently sized asset')
                record = published
            except (RuntimeError, ValueError, KeyError, TypeError) as error:
                warning = f"{release['tag_name']}: reserved, not installable: {error}"
        return addon_id, record, warning

    warnings = []
    with ThreadPoolExecutor(max_workers=6) as pool:
        for addon_id, record, warning in pool.map(convert, releases):
            addon = addons.setdefault(addon_id, {'id': addon_id, 'path': f'addons/{addon_id}', 'versions': []})
            addon['versions'].append(record)
            if warning:
                warnings.append(warning)
    return {'schema': 1, 'addons': list(addons.values())}, warnings


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', default='franchet-ovh/NewCode')
    parser.add_argument('--output', type=Path, default=Path('addons.json'))
    args = parser.parse_args()
    if args.output.exists():
        parser.error('Output already exists; choose another --output and review the diff before replacing the index')
    index, warnings = import_history(args.repo)
    args.output.write_text(encode(index))
    for warning in warnings:
        print(warning, file=sys.stderr)
    print(f"Imported {sum(len(a['versions']) for a in index['addons'])} versions into {args.output}")


if __name__ == '__main__':
    main()
