"""Editor release orchestration without network calls, publishing, or local version edits."""
from contextlib import ExitStack, redirect_stderr, redirect_stdout
import io
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

import release_editor as cli


SHA = 'a' * 40


class ReleaseEditorTests(unittest.TestCase):
    def invoke(self, args=(), source='0.4.9', pages=None, failure=None, available=True,
               addon_status=0, addon_error=None, sha=SHA, manifest=None):
        replies = ['', sha, manifest or f'[package]\nname="focus-editor"\nversion="{source}"',
                   '\n'.join(json.dumps(page) for page in (pages if pages is not None else [[]])), '']
        if failure:
            replies[failure[0]] = RuntimeError(failure[1])
        with ExitStack() as stack:
            stack.enter_context(patch.object(cli.shutil, 'which', return_value='/bin/gh' if available else None))
            gh = stack.enter_context(patch.object(cli, 'gh', side_effect=replies))
            addon = stack.enter_context(patch.object(cli.subprocess, 'run',
                side_effect=addon_error, return_value=subprocess.CompletedProcess([], addon_status)))
            prompt = stack.enter_context(patch('builtins.input', side_effect=AssertionError('Unexpected prompt')))
            out, err = io.StringIO(), io.StringIO()
            stack.enter_context(redirect_stdout(out))
            stack.enter_context(redirect_stderr(err))
            code = cli.main(list(args))
            prompt.assert_not_called()
        return code, out.getvalue(), err.getvalue(), gh, addon

    def assert_no_publication(self, gh, addon):
        self.assertFalse(any(call.args[:2] == ('release', 'create') for call in gh.call_args_list))
        addon.assert_not_called()

    def test_default_publishes_current_main_version_and_does_not_release_addons(self):
        code, out, err, gh, addon = self.invoke(pages=[[{'tag_name': 'v0.4.8'}]])
        self.assertEqual((code, err), (0, ''))
        gh.assert_any_call('api', f'repos/{cli.DEFAULT_REPO}/contents/Cargo.toml?ref={SHA}',
                           '-H', 'Accept: application/vnd.github.raw+json')
        self.assertEqual(gh.call_args.args,
            ('release', 'create', 'v0.4.9', '--repo', cli.DEFAULT_REPO, '--target', SHA,
             '--title', 'v0.4.9', '--generate-notes'))
        self.assertIn('publishing has not finished', out)
        self.assertIn(f'https://github.com/{cli.DEFAULT_REPO}/actions/workflows/release.yml', out)
        addon.assert_not_called()

    def test_bumps_share_addon_semver_logic_and_ignore_addon_and_prerelease_tags(self):
        pages = [[{'tag_name': 'v0.5.9'}, {'tag_name': 'addon_github_99.0.0'},
                  {'tag_name': 'v9.0.0-rc.1'}],
                 [{'tag_name': '0.6.2', 'draft': True}]]
        for flag, tag in [('--patch', 'v0.6.3'), ('--minor', 'v0.7.0'), ('--major', 'v1.0.0')]:
            with self.subTest(flag=flag):
                code, _, err, gh, addon = self.invoke([flag], pages=pages)
                self.assertEqual((code, err), (0, ''))
                self.assertEqual(gh.call_args.args[2], tag)
                gh.assert_any_call('api', '--paginate', f'repos/{cli.DEFAULT_REPO}/releases?per_page=100')
                addon.assert_not_called()

    def test_explicit_version_can_equal_unpublished_source_but_never_go_backwards(self):
        code, _, err, gh, _ = self.invoke(['--version', '0.4.9'])
        self.assertEqual((code, err), (0, ''))
        self.assertEqual(gh.call_args.args[2], 'v0.4.9')
        for args, pages in [([], [[{'tag_name': 'v0.4.9', 'draft': True}]]),
                            (['--version', '0.4.8'], [[]]),
                            (['--version', '0.5.0'], [[{'tag_name': '0.5.0'}]])]:
            with self.subTest(args=args, pages=pages):
                code, _, err, gh, addon = self.invoke(args, pages=pages)
                self.assertEqual(code, 1)
                self.assertIn('Select a newer version', err)
                self.assert_no_publication(gh, addon)

    def test_with_addon_calls_existing_cli_after_editor_publication(self):
        events = []
        def publish(*args):
            events.append('auth' if args[:2] == ('auth', 'status') else 'editor')
            return ''
        def addons(*args, **kwargs):
            events.append('addons')
            return subprocess.CompletedProcess([], 0)
        with patch.object(cli.shutil, 'which', return_value='/bin/gh'), \
             patch.object(cli, 'source_version', return_value=(SHA, '0.4.9')), \
             patch.object(cli, 'release_versions', return_value=[]), \
             patch.object(cli, 'gh', side_effect=publish), \
             patch.object(cli.subprocess, 'run', side_effect=addons) as child, \
             redirect_stdout(io.StringIO()):
            self.assertEqual(cli.main(['--with-addon', '--repo', 'owner/project']), 0)
        self.assertEqual(events, ['auth', 'editor', 'addons'])
        child.assert_called_once_with([cli.sys.executable, str(cli.ADDON_SCRIPT),
                                       '--all', '--minor', '--repo', 'owner/project'], check=False)

    def test_editor_bump_does_not_change_requested_addon_minor_bump(self):
        code, _, err, gh, addon = self.invoke(['--major', '--with-addon'])
        self.assertEqual((code, err), (0, ''))
        self.assertEqual(gh.call_args.args[2], 'v1.0.0')
        self.assertEqual(addon.call_args.args[0],
            [cli.sys.executable, str(cli.ADDON_SCRIPT), '--all', '--minor', '--repo', cli.DEFAULT_REPO])

    def test_publication_failure_never_starts_addons_and_explains_uncertain_status(self):
        code, out, err, gh, addon = self.invoke(['--with-addon'], failure=(4, 'network interrupted'))
        self.assertEqual(code, 1)
        self.assertIn('Could not confirm editor release creation', err)
        self.assertIn('/releases/tag/v0.4.9', err)
        self.assertIn('No addon releases were attempted', err)
        self.assertNotIn('Editor release created:', out)
        addon.assert_not_called()

    def test_addon_failure_keeps_editor_success_visible_and_stops(self):
        for kwargs in [{'addon_status': 1}, {'addon_error': OSError('cannot execute child')}]:
            with self.subTest(kwargs=kwargs):
                code, out, err, gh, addon = self.invoke(['--with-addon'], **kwargs)
                self.assertEqual(code, 1)
                self.assertIn('Editor release created:', out)
                self.assertIn('Editor release was created:', err)
                self.assertIn('/releases/tag/v0.4.9', err)
                self.assertEqual(sum(call.args[:2] == ('release', 'create') for call in gh.call_args_list), 1)
                addon.assert_called_once()

    def test_invalid_main_or_missing_credentials_stop_before_publication(self):
        for kwargs in [{'available': False}, {'failure': (0, 'not authenticated')}, {'sha': 'main'},
                       {'manifest': '[package]\nname="other"\nversion="0.4.9"'},
                       {'source': '0.4.9-rc.1'}, {'manifest': '[package]\nname="focus-editor"\nversion=true'}]:
            with self.subTest(kwargs=kwargs):
                code, _, err, gh, addon = self.invoke(**kwargs)
                self.assertEqual(code, 1)
                self.assertTrue(err)
                self.assert_no_publication(gh, addon)

    def test_invalid_arguments_and_help_never_contact_github(self):
        for args, expected in [(['--minor', '--patch'], 2), (['--version', '1.0.0', '--major'], 2),
                               (['--repo', 'not/a/repo'], 2), (['--help'], 0)]:
            with self.subTest(args=args), patch.object(cli, 'gh') as gh, \
                 redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()), \
                 self.assertRaises(SystemExit) as error:
                cli.main(args)
            self.assertEqual(error.exception.code, expected)
            gh.assert_not_called()
        for version in ['v1.2.3', '1.2', '1.2.3-rc.1', '1.02.3']:
            code, _, err, gh, addon = self.invoke(['--version', version])
            self.assertEqual(code, 1)
            self.assertIn('Editor releases require a stable', err)
            self.assert_no_publication(gh, addon)

    def test_local_versions_are_not_modified(self):
        root = Path(__file__).resolve().parents[2]
        files = [root/'Cargo.toml', root/'Cargo.lock', *root.glob('addons/*/addon.toml')]
        before = {path: path.read_bytes() for path in files}
        code, _, err, _, _ = self.invoke(['--patch', '--with-addon'])
        self.assertEqual((code, err), (0, ''))
        self.assertEqual(before, {path: path.read_bytes() for path in files})


if __name__ == '__main__':
    unittest.main()
