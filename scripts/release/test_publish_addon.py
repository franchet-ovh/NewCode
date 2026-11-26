"""Independent versioning and packaging behavior without GitHub requests."""
import json
from pathlib import Path
import shutil
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import tomllib
from addon_release import RELEASE_TARGETS, assets, manifests, package_addons, editor_compatibility
import publish_addon as publisher


class AddonReleaseTests(unittest.TestCase):
    def test_editor_bounds_preserve_omitted_end_and_validate_ranges(self):
        manifest = {'editor_version_req': '>=0.4.8, <0.5.0'}
        for lower, upper, expected in [
            ('', '', '>=0.4.8, <0.5.0'),
            ('0.4.9', '', '>=0.4.9, <0.5.0'),
            ('', '0.6.0', '>=0.4.8, <0.6.0'),
            ('0.5.0', '0.6.0', '>=0.5.0, <0.6.0'),
        ]:
            self.assertEqual(editor_compatibility(manifest, lower, upper), expected)
        for lower, upper in [('0.5.0', ''), ('', '0.4.8'), ('bad', ''), ('', '01.6.0'),
                             ('', '0.6.0-rc.1'), ('0.6.0', '0.5.0')]:
            with self.assertRaises(ValueError):
                editor_compatibility(manifest, lower, upper)
        self.assertEqual(editor_compatibility({'editor_version_req': '^0.5'}), '^0.5')
        with self.assertRaises(ValueError):
            editor_compatibility({'editor_version_req': '^0.5'}, upper='0.6.0')

    def test_stamped_editor_bounds_reach_archives_and_catalogs(self):
        for lower, upper, expected in [('', '', '>=0.4.8, <0.5.0'),
                                       ('0.4.9', '', '>=0.4.9, <0.5.0'),
                                       ('', '0.6.0', '>=0.4.8, <0.6.0'),
                                       ('0.5.0', '0.6.0', '>=0.5.0, <0.6.0')]:
            with tempfile.TemporaryDirectory() as temporary, patch.object(publisher, 'ROOT', Path(temporary)):
                root = Path(temporary)
                self.fixture(root)
                other = {p: p.read_bytes() for p in (root / 'addons/second').iterdir()}
                publisher.stamp('first', 'addon_first_0.4.9', lower, upper)
                self.assertEqual(other, {p: p.read_bytes() for p in other})
                manifest_path, manifest = publisher.definition('first')
                self.assertEqual(manifest['editor_version_req'], expected)
                for target in RELEASE_TARGETS:
                    source = root / 'build' / target / 'addons/first'
                    source.mkdir(parents=True)
                    (source / 'addon.toml').write_text(f'target="{target}"\n' + manifest_path.read_text())
                    for asset in assets(manifest):
                        if asset != 'addon.toml':
                            (source / asset).write_bytes(b'fixture')
                    catalog = package_addons(root / 'build' / target, root / 'dist', 'addon_first_0.4.9', target, root, ['first'])
                    package = catalog['packages'][0]
                    self.assertEqual(package['editor_version_req'], expected)
                    with tarfile.open(root / 'dist' / package['archive']) as archive:
                        self.assertEqual(tomllib.loads(archive.extractfile('addon.toml').read().decode())['editor_version_req'], expected)
                publisher.finalize('first', 'addon_first_0.4.9', root / 'dist')

    def test_reservation_distinguishes_editor_overrides_and_reuses_matching_retry(self):
        manifest = {'id': 'first', 'version': '0.4.8', 'editor_version_req': '>=0.4.8, <0.5.0'}
        releases = [{'tag_name': 'addon_first_0.4.9', 'body': 'Addon source: ' + 'a' * 40 + '\nAddon inputs: fingerprint', 'draft': False}]
        with patch.object(publisher, 'definition', return_value=(None, manifest)), patch.object(publisher, 'fingerprint', return_value='fingerprint'), patch.dict('os.environ', GH_REPO='owner/repo'):
            with patch.object(publisher, 'gh', side_effect=[json.dumps(releases), '']) as gh:
                self.assertEqual(publisher.reserve('first', 'a' * 40, '', upper='0.6.0'), ('addon_first_0.4.10', False))
                body = gh.call_args.args[gh.call_args.args.index('--notes') + 1]
                self.assertIn('Editor compatibility override: >=0.4.8, <0.6.0', body)
            releases = [{'tag_name': 'addon_first_0.4.10', 'body': body, 'draft': True}]
            with patch.object(publisher, 'gh', return_value=json.dumps(releases)) as gh:
                self.assertEqual(publisher.reserve('first', 'a' * 40, '', upper='0.6.0'), ('addon_first_0.4.10', False))
                gh.assert_called_once()
            for upper in ['', '0.7.0']:
                with patch.object(publisher, 'gh', side_effect=[json.dumps(releases), '']):
                    self.assertEqual(publisher.reserve('first', 'a' * 40, '', upper=upper), ('addon_first_0.4.11', False))

    def test_missing_compatibility_fails_before_querying_or_reserving_a_release(self):
        for requirement in [None, '', '   ', 42]:
            manifest = {'id': 'first', 'version': '0.4.8', 'editor_version_req': requirement}
            with patch.object(publisher, 'definition', return_value=(None, manifest)), patch.object(publisher, 'gh') as gh:
                with self.assertRaisesRegex(ValueError, 'first.*editor_version_req'):
                    publisher.reserve('first', 'a' * 40, '')
                gh.assert_not_called()

    def test_all_bundled_addons_have_compatibility_metadata_for_both_release_targets(self):
        repo = publisher.ROOT
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            definitions = manifests(repo)
            self.assertTrue({'lsp-python', 'lsp-rust'}.issubset({m['id'] for _, m in definitions}))
            for path, manifest in definitions:
                for target in RELEASE_TARGETS:
                    with self.subTest(addon=manifest['id'], target=target):
                        build = root / target
                        stage = build / 'addons' / manifest['id']
                        stage.mkdir(parents=True)
                        (stage / 'addon.toml').write_text(f'target="{target}"\n' + path.read_text())
                        for asset in assets(manifest):
                            if asset == 'addon.toml':
                                continue
                            destination = stage / asset
                            destination.parent.mkdir(parents=True, exist_ok=True)
                            if asset == manifest['executable']:
                                destination.write_bytes(b'fixture executable')
                            else:
                                source = path.parent / asset
                                shutil.copyfile(source if source.is_file() else repo / asset, destination)
                        tag = f"addon_{manifest['id']}_{manifest['version']}"
                        output = root / 'dist' / target / manifest['id']
                        catalog = package_addons(build, output, tag, target, repo, [manifest['id']])
                        package = catalog['packages'][0]
                        self.assertEqual(catalog['schema'], 2)
                        self.assertEqual(package['editor_version_req'], manifest['editor_version_req'])
                        self.assertTrue(package['editor_version_req'].strip())
                        with tarfile.open(output / package['archive']) as archive:
                            packaged = tomllib.loads(archive.extractfile('addon.toml').read().decode())
                        self.assertEqual(packaged['editor_version_req'], manifest['editor_version_req'])
                        # Keep rejecting a staged manifest that changes compatibility.
                        text = (stage / 'addon.toml').read_text().replace(manifest['editor_version_req'], '>=9.0.0')
                        (stage / 'addon.toml').write_text(text)
                        with self.assertRaisesRegex(ValueError, 'staged editor compatibility range'):
                            package_addons(build, output, tag, target, repo, [manifest['id']])

    def test_release_pagination_supports_older_gh_json_streams(self):
        pages = [[{'tag_name': 'addon_first_0.4.9', 'body': 'brackets [] and newlines\n'}],
                 [{'tag_name': 'addon_first_0.4.10', 'draft': True}], []]
        for output in ['\n'.join(json.dumps(page, indent=2) for page in pages),
                       ''.join(json.dumps(page) for page in pages)]:
            with patch.object(publisher, 'gh', return_value=' \n' + output + '\n ') as gh:
                self.assertEqual(publisher.release_pages('owner/repo'), pages)
                gh.assert_called_once_with('api', '--paginate', 'repos/owner/repo/releases?per_page=100')
        with patch.object(publisher, 'gh', return_value='[]'):
            self.assertEqual(publisher.release_pages('owner/repo'), [[]])

    def test_invalid_or_incomplete_release_pages_fail_instead_of_ignoring_versions(self):
        for output in ['', ' \n ', '{"message":"error"}', '[]\n[{', '[]\ntruncated']:
            with patch.object(publisher, 'gh', return_value=output):
                with self.assertRaises(ValueError):
                    publisher.release_pages('owner/repo')

    def test_versions(self):
        self.assertEqual(publisher.next_version('0.4.8', []), '0.4.9')
        self.assertEqual(publisher.next_version('0.4.8', ['0.4.9', '0.5.2']), '0.5.3')
        self.assertEqual(publisher.next_version('0.6.0', ['0.5.2']), '0.6.1')
        self.assertEqual(publisher.next_version('0.4.8', ['0.4.9'], '1.0.0'), '1.0.0')
        for invalid in ['0.4.8', '0.4.9', '1.0.0-1', '1.0.0+build', '01.0.0']:
            with self.assertRaises(ValueError):
                publisher.next_version('0.4.8', ['0.4.9'], invalid)

    def fixture(self, root):
        for name in ['first', 'second']:
            path = root / 'addons' / name
            path.mkdir(parents=True)
            (path / 'addon.toml').write_text(f'id="{name}"\nname="{name}"\nversion="0.4.8"\neditor_version_req=">=0.4.8, <0.5.0"\nprotocol=1\nexecutable="program"\nicon="icon.png"\n')
            (path / 'Cargo.toml').write_text(f'[package]\nname="focus-addon-{name}"\nversion="0.4.8"\n')
            (path / 'Cargo.lock').write_text(f'version=4\n[[package]]\nname="focus-addon-{name}"\nversion="0.4.8"\n')
        (root / 'Cargo.toml').write_text('[package]\nversion="0.4.8"\n')

    def test_isolated_stamp_and_complete_assets(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(publisher, 'ROOT', Path(temporary)):
            root = Path(temporary)
            self.fixture(root)
            other = {p: p.read_bytes() for p in (root / 'addons/second').iterdir()}
            publisher.stamp('first', 'addon_first_0.4.9')
            self.assertEqual(tomllib.loads((root / 'Cargo.toml').read_text())['package']['version'], '0.4.8')
            self.assertEqual(other, {p: p.read_bytes() for p in other})
            manifest = publisher.definition('first')[1]
            for target in RELEASE_TARGETS:
                source = root / 'build' / target / 'addons/first'
                source.mkdir(parents=True)
                (source / 'addon.toml').write_text(f'target="{target}"\n' + (root / 'addons/first/addon.toml').read_text())
                for asset in assets(manifest):
                    if asset != 'addon.toml':
                        (source / asset).write_bytes(b'fixture')
                package_addons(root / 'build' / target, root / 'dist', 'addon_first_0.4.9', target, root, ['first'])
            publisher.finalize('first', 'addon_first_0.4.9', root / 'dist')
            catalog = json.loads((root / 'dist/addons.json').read_text())
            self.assertEqual(catalog['schema'], 2)
            self.assertEqual([p['id'] for p in catalog['packages']], ['first', 'first'])
            self.assertTrue((root / 'dist/SHA256SUMS').is_file())
            (root / 'dist' / catalog['packages'][0]['archive']).write_bytes(b'corrupted')
            with self.assertRaisesRegex(ValueError, 'checksum'):
                publisher.finalize('first', 'addon_first_0.4.9', root / 'dist')

    def test_reservation_reuses_draft_and_completed_release(self):
        for draft in [True, False]:
            release = {'tag_name': 'addon_first_0.4.9', 'body': 'Addon source: ' + 'a' * 40, 'draft': draft}
            with patch.object(publisher, 'fingerprint', return_value='fingerprint'), patch.object(publisher, 'definition', return_value=(None, {'id': 'first', 'version': '0.4.8', 'editor_version_req': '>=0.4.8, <0.5.0'})), patch.dict('os.environ', GH_REPO='owner/repo'), patch.object(publisher, 'gh', return_value=json.dumps([release])) as gh:
                self.assertEqual(publisher.reserve('first', 'a' * 40, ''), ('addon_first_0.4.9', not draft))
                gh.assert_called_once()

    def test_unchanged_inputs_skip_only_the_latest_published_version(self):
        releases = [
            {'tag_name': 'addon_first_0.4.10', 'body': 'Addon inputs: newer', 'draft': False},
            {'tag_name': 'addon_first_0.4.9', 'body': 'Addon inputs: old', 'draft': False},
        ]
        with patch.object(publisher, 'definition', return_value=(None, {'id': 'first', 'version': '0.4.8', 'editor_version_req': '>=0.4.8, <0.5.0'})), patch.dict('os.environ', GH_REPO='owner/repo'):
            with patch.object(publisher, 'fingerprint', return_value='newer'), patch.object(publisher, 'gh', return_value=json.dumps(releases)) as gh:
                self.assertEqual(publisher.reserve('first', 'a' * 40, ''), ('addon_first_0.4.10', True))
                gh.assert_called_once()
            # Reverting to older inputs still needs a NEW release to reach users.
            with patch.object(publisher, 'fingerprint', return_value='old'), patch.object(publisher, 'gh', side_effect=[json.dumps(releases), '']) as gh:
                self.assertEqual(publisher.reserve('first', 'a' * 40, ''), ('addon_first_0.4.11', False))
                self.assertEqual(gh.call_count, 2)


if __name__ == '__main__':
    unittest.main()
