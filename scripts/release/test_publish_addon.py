"""Independent versioning and packaging behavior without GitHub requests."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import tomllib
from addon_release import RELEASE_TARGETS, assets, package_addons
import publish_addon as publisher


class AddonReleaseTests(unittest.TestCase):
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
            with patch.object(publisher, 'fingerprint', return_value='fingerprint'), patch.object(publisher, 'definition', return_value=(None, {'version': '0.4.8'})), patch.dict('os.environ', GH_REPO='owner/repo'), patch.object(publisher, 'gh', return_value=json.dumps([[release]])) as gh:
                self.assertEqual(publisher.reserve('first', 'a' * 40, ''), ('addon_first_0.4.9', not draft))
                gh.assert_called_once()

    def test_unchanged_inputs_skip_only_the_latest_published_version(self):
        releases = [
            {'tag_name': 'addon_first_0.4.10', 'body': 'Addon inputs: newer', 'draft': False},
            {'tag_name': 'addon_first_0.4.9', 'body': 'Addon inputs: old', 'draft': False},
        ]
        with patch.object(publisher, 'definition', return_value=(None, {'version': '0.4.8'})), patch.dict('os.environ', GH_REPO='owner/repo'):
            with patch.object(publisher, 'fingerprint', return_value='newer'), patch.object(publisher, 'gh', return_value=json.dumps([releases])) as gh:
                self.assertEqual(publisher.reserve('first', 'a' * 40, ''), ('addon_first_0.4.10', True))
                gh.assert_called_once()
            # Reverting to older inputs still needs a NEW release to reach users.
            with patch.object(publisher, 'fingerprint', return_value='old'), patch.object(publisher, 'gh', side_effect=[json.dumps([releases]), '']) as gh:
                self.assertEqual(publisher.reserve('first', 'a' * 40, ''), ('addon_first_0.4.11', False))
                self.assertEqual(gh.call_count, 2)


if __name__ == '__main__':
    unittest.main()
