"""Durable release selection, publication history, import, and concurrent registration."""
import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import addon_index as registry
from addon_release import RELEASE_TARGETS
from github_api import GitHubError
from import_addon_index import import_history


def fixture():
    return {'schema': 1, 'addons': [{'id': 'fixture', 'path': 'addons/fixture', 'versions': [{
        'version': '1.0.0', 'release': 'addon_fixture_1.0.0', 'status': 'planned',
        'protocol': 1, 'editor_version_req': '>=0.5.0, <0.6.0', 'packages': []}]}]}


def catalog():
    return {'schema': 2, 'release': 'addon_fixture_1.0.0', 'packages': [{
        'id': 'fixture', 'name': 'Fixture', 'version': '1.0.0', 'protocol': 1,
        'editor_version_req': '>=0.5.0, <0.6.0', 'target': target,
        'archive': f'fixture-{target}.tar.gz', 'size': 10, 'sha256': 'a'*64,
    } for target in RELEASE_TARGETS]}


class IndexTests(unittest.TestCase):
    def source(self, root):
        path = root/'addons/fixture'
        path.mkdir(parents=True)
        (path/'addon.toml').write_text('id="fixture"\nname="Fixture"\nversion="1.0.0"\nprotocol=1\neditor_version_req=">=0.5.0, <0.6.0"\nexecutable="program"\nicon="icon.png"\n')
        (path/'Cargo.toml').write_text('[package]\nname="fixture"\nversion="1.0.0"\n')
        (path/'Cargo.lock').write_text('version=4\n[[package]]\nname="fixture"\nversion="1.0.0"\n')

    def test_durable_plans_select_retries_and_index_only_commits_select_nothing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.source(root)
            index = fixture()
            self.assertEqual(registry.validate_source(root, index), ['fixture'])
            # Unrelated code changes cannot lose release intent.
            (root/'addons/fixture/code.rs').write_text('changed')
            self.assertEqual(registry.validate_source(root, index), ['fixture'])
            published = registry.register(index, 'fixture', 'addon_fixture_1.0.0', catalog())
            self.assertEqual(registry.validate_source(root, published), [])
            index['addons'][0]['versions'][0]['version'] = '1.0.1'
            index['addons'][0]['versions'][0]['release'] = 'addon_fixture_1.0.1'
            with self.assertRaisesRegex(ValueError, 'differs from committed manifest'):
                registry.validate_source(root, index)

    def test_registration_preserves_other_versions_and_is_idempotent(self):
        index = fixture()
        previous = copy.deepcopy(index)
        published = registry.register(index, 'fixture', 'addon_fixture_1.0.0', catalog())
        self.assertEqual(index, previous)
        # Exercise the actual write/read boundary: encoding sorts package targets.
        saved = json.loads(registry.encode(published))
        self.assertEqual(saved, registry.register(saved, 'fixture', 'addon_fixture_1.0.0', catalog()))
        next_version = dict(index['addons'][0]['versions'][0], version='1.1.0', release='addon_fixture_1.1.0')
        index['addons'][0]['versions'].append(next_version)
        updated = registry.register(index, 'fixture', 'addon_fixture_1.0.0', catalog())
        self.assertEqual(registry.record(updated, 'fixture', '1.1.0'), next_version)
        changed = catalog()
        changed['packages'][0]['sha256'] = 'b'*64
        with self.assertRaisesRegex(ValueError, 'cannot be replaced'):
            registry.register(published, 'fixture', 'addon_fixture_1.0.0', changed)
        incomplete = catalog()
        incomplete['packages'].pop()
        with self.assertRaisesRegex(ValueError, 'Both release targets'):
            registry.register(index, 'fixture', 'addon_fixture_1.0.0', incomplete)

    def test_history_cannot_be_deleted_or_reused(self):
        before = registry.register(fixture(), 'fixture', 'addon_fixture_1.0.0', catalog())
        after = copy.deepcopy(before)
        after['addons'][0]['versions'].append(dict(fixture()['addons'][0]['versions'][0], version='1.0.1', release='addon_fixture_1.0.1'))
        registry.validate_transition(before, after)
        for change in ['remove', 'replace', 'older']:
            after = copy.deepcopy(before)
            if change == 'remove':
                after['addons'][0]['versions'] = []
            elif change == 'replace':
                after['addons'][0]['versions'][0]['packages'][0]['sha256'] = 'b'*64
            else:
                after['addons'][0]['versions'].append(dict(fixture()['addons'][0]['versions'][0], version='0.9.0', release='addon_fixture_0.9.0'))
            with self.assertRaises(ValueError):
                registry.validate_transition(before, after)

    def test_concurrent_publication_rereads_and_merges_latest_index(self):
        index = fixture()
        latest = copy.deepcopy(index)
        latest['addons'].append({'id': 'other', 'path': 'addons/other', 'versions': []})
        written = []
        def query(*args, input_data=None):
            if args[0] == 'release':
                return json.dumps(catalog())
            written.append(json.loads(input_data))
            if len(written) == 1:
                raise GitHubError('conflict (HTTP 409)')
            return '{}'
        with patch.object(registry, 'api', return_value={'draft': False, 'prerelease': False}), patch.object(registry, 'remote', side_effect=[(index, 'old-blob'), (latest, 'new-blob')]), patch.object(registry, 'gh', side_effect=query), patch.object(registry.time, 'sleep'):
            registry.update_remote('owner/repo', 'fixture', 'addon_fixture_1.0.0')
        self.assertEqual([body['sha'] for body in written], ['old-blob', 'new-blob'])
        import base64
        final = json.loads(base64.b64decode(written[-1]['content']))
        self.assertEqual(registry.entry(final, 'other'), latest['addons'][1])
        self.assertEqual(registry.record(final, 'fixture', '1.0.0')['status'], 'published')

    def test_api_failure_and_private_release_do_not_modify_index(self):
        for release in [{'draft': True, 'prerelease': False}, {'draft': False, 'prerelease': True}]:
            with patch.object(registry, 'api', return_value=release), patch.object(registry, 'gh') as query:
                with self.assertRaises(ValueError):
                    registry.update_remote('owner/repo', 'fixture', 'addon_fixture_1.0.0')
                query.assert_not_called()
        with patch.object(registry, 'api', side_effect=GitHubError('forbidden (HTTP 403)')), patch.object(registry, 'remote') as remote:
            with self.assertRaises(GitHubError):
                registry.update_remote('owner/repo', 'fixture', 'addon_fixture_1.0.0')
            remote.assert_not_called()

    def test_invalid_metadata_is_not_installable(self):
        published = registry.register(fixture(), 'fixture', 'addon_fixture_1.0.0', catalog())
        for field, value in [('id', 'other'), ('version', '9.0.0'), ('editor_version_req', '>=99.0.0'), ('archive', '../escape.tar.gz'), ('size', 0), ('sha256', 'bad'), ('source_release', 'wrong')]:
            index = copy.deepcopy(published)
            index['addons'][0]['versions'][0]['packages'][0][field] = value
            with self.assertRaises(ValueError):
                registry.validate(index)
        planned = fixture()
        planned['addons'][0]['versions'][0]['packages'] = catalog()['packages']
        with self.assertRaises(ValueError):
            registry.validate(planned)

    def test_import_reserves_drafts_and_broken_releases_without_advertising_them(self):
        releases = [{'tag_name': f'addon_fixture_{version}', 'draft': draft, 'prerelease': False, 'assets': [
            {'name': p['archive'], 'size': p['size']} for p in catalog()['packages']]} for version, draft in [('1.0.0', False), ('1.1.0', True), ('0.9.0', False)]]
        def query(*args):
            if args[0] == 'api':
                return json.dumps(releases)
            if args[2] == 'addon_fixture_0.9.0':
                raise RuntimeError('missing catalog')
            return json.dumps(catalog())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.source(root)
            index, warnings = import_history('owner/repo', root, query)
        self.assertEqual(registry.record(index, 'fixture', '1.0.0')['status'], 'published')
        self.assertEqual(registry.record(index, 'fixture', '1.1.0')['status'], 'reserved')
        self.assertEqual(registry.record(index, 'fixture', '0.9.0')['status'], 'reserved')
        self.assertEqual(len(warnings), 1)
        registry.validate(index)

    def test_large_remote_index_uses_raw_content_and_keeps_the_file_revision(self):
        with patch.object(registry, 'api', return_value={'encoding': 'none', 'sha': 'file-revision'}), patch.object(registry, 'gh', return_value=json.dumps(fixture())) as query:
            index, sha = registry.remote('owner/repo', 'commit')
        self.assertEqual((index, sha), (fixture(), 'file-revision'))
        self.assertIn('Accept: application/vnd.github.raw+json', query.call_args.args)

    def test_superseded_plans_remain_reserved_and_cannot_be_reused(self):
        before = fixture()
        after = copy.deepcopy(before)
        after['addons'][0]['versions'][0]['status'] = 'reserved'
        after['addons'][0]['versions'].append(dict(before['addons'][0]['versions'][0], version='1.0.1', release='addon_fixture_1.0.1'))
        registry.validate_transition(before, after)
        with self.assertRaisesRegex(ValueError, 'matching planned/published'):
            registry.register(after, 'fixture', 'addon_fixture_1.0.0', catalog())


if __name__ == '__main__':
    unittest.main()
