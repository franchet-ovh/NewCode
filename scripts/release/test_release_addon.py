"""Release selection and workflow dispatch without network calls or publishing."""
from contextlib import ExitStack, redirect_stdout, redirect_stderr
import io
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

import release_addon as cli


SHA = 'a' * 40
LEGACY_WORKFLOW = '''on:
  workflow_call:
    inputs:
      source-sha:
        type: string
  workflow_dispatch:
    inputs:
      addon:
        type: choice
        options: [github, codex, opencode]
      version:
        type: string
jobs:
  prepare:
    steps: []
'''
FREEFORM_WORKFLOW = LEGACY_WORKFLOW.replace('type: choice\n        options: [github, codex, opencode]', 'type: string')
PINNED_WORKFLOW = FREEFORM_WORKFLOW.replace('      version:', '      source-sha:\n        type: string\n      version:')
TREE = {'tree': [
    {'path': f'addons/{addon}/addon.toml', 'type': 'blob'}
    for addon in ('github', 'codex', 'lsp-rust')
] + [{'path': 'addons/not-a-manifest/README.md', 'type': 'blob'}]}


def release(tag, draft=False):
    return {'tag_name': tag, 'draft': draft}


def page_stream(pages):
    return '\n'.join(json.dumps(page, indent=2) for page in pages)


class ReleaseAddonTests(unittest.TestCase):
    def invoke(self, args=(), answers=(), interactive=True, releases=None, source='0.4.8',
               addon='github', failure=None, available=True, responses=None, workflow=PINNED_WORKFLOW):
        replies = list(responses) if responses is not None else [
            '', SHA, json.dumps(TREE), f'id="{addon}"\nversion="{source}"\neditor_version_req=">=0.4.8, <0.5.0"',
            page_stream(releases or [[]]), workflow, '']
        if failure:
            replies[failure[0]] = RuntimeError(failure[1])
        with ExitStack() as stack:
            stack.enter_context(patch.object(cli.shutil, 'which', return_value='/bin/gh' if available else None))
            stack.enter_context(patch.object(cli.sys.stdin, 'isatty', return_value=interactive))
            stack.enter_context(patch('builtins.input', side_effect=answers))
            gh = stack.enter_context(patch.object(cli, 'gh', side_effect=replies))
            out, err = io.StringIO(), io.StringIO()
            stack.enter_context(redirect_stdout(out))
            stack.enter_context(redirect_stderr(err))
            code = cli.main(list(args))
        return code, out.getvalue(), err.getvalue(), gh

    def assert_no_dispatch(self, gh):
        self.assertFalse(any(call.args[:2] == ('workflow', 'run') for call in gh.call_args_list))

    def test_missing_compatibility_in_remote_manifest_stops_the_whole_batch(self):
        replies = self.batch_replies()
        # The last addon lacks compatibility metadata, matching the failed runs.
        replies[6] = 'id="lsp-rust"\nversion="0.4.8"'
        code, _, err, gh = self.invoke(['--all', '--patch'], interactive=False, responses=replies)
        self.assertEqual(code, 1)
        self.assertIn('lsp-rust must declare a nonempty editor_version_req', err)
        self.assert_no_dispatch(gh)

    def batch_replies(self, selected=('codex', 'github', 'lsp-rust'), workflow=PINNED_WORKFLOW):
        sources = {'codex': '0.4.8', 'github': '1.5.0', 'lsp-rust': '0.2.0'}
        pages = [[release('addon_codex_0.8.9', draft=True), release('addon_github_2.3.4')],
                 [release('addon_lsp-rust_0.1.5')]]
        replies = ['', SHA, json.dumps(TREE)]
        for index, addon in enumerate(selected):
            replies.append(f'id="{addon}"\nversion="{sources[addon]}"\neditor_version_req=">=0.4.8, <0.5.0"')
            if index == 0:
                replies.append(page_stream(pages))
        return replies + [workflow] + [''] * len(selected)

    def test_manual_workflow_inputs_do_not_include_reusable_trigger_inputs(self):
        self.assertEqual(set(cli.dispatch_inputs(LEGACY_WORKFLOW)), {'addon', 'version'})
        self.assertEqual(set(cli.dispatch_inputs(PINNED_WORKFLOW)), {'addon', 'version', 'source-sha'})
        self.assertEqual(cli.dispatch_inputs(LEGACY_WORKFLOW)['addon'],
                         {'type': 'choice', 'options': ['github', 'codex', 'opencode']})
        with_comments = LEGACY_WORKFLOW.replace('      addon:', '      # source-sha is not supported\n\n      addon:')
        self.assertEqual(set(cli.dispatch_inputs(with_comments)), {'addon', 'version'})
        self.assertEqual(cli.dispatch_inputs('on:\n  workflow_call:\n    inputs:\n      source-sha:\n'), {})

    def test_choice_lists_support_quotes_comments_and_block_sequences(self):
        for options in [
            "['github', \"codex\", lsp-python, lsp-rust] # release targets",
            "\n          - github\n          - 'codex' # assistant\n          - lsp-python\n          - lsp-rust",
            "\n        - github\n        - codex\n        - lsp-python\n        - lsp-rust",
        ]:
            workflow = LEGACY_WORKFLOW.replace('[github, codex, opencode]', options)
            inputs = cli.dispatch_inputs(workflow)
            self.assertEqual(inputs['addon']['options'], ['github', 'codex', 'lsp-python', 'lsp-rust'])
            cli.validate_choices(inputs, 'addon', ['lsp-python', 'lsp-rust'])

    def test_local_workflow_change_enables_all_addons(self):
        path = Path(__file__).resolve().parents[2] / '.github/workflows/addon-release.yml'
        inputs = cli.dispatch_inputs(path.read_text())
        self.assertEqual(inputs['addon']['type'], 'string')
        cli.validate_choices(inputs, 'addon', ['codex', 'github', 'lsp-python', 'lsp-rust', 'opencode'])

    def test_existing_main_workflow_receives_only_supported_inputs(self):
        code, out, err, gh = self.invoke(['--addon', 'codex', '--patch'], interactive=False,
                                       addon='codex', workflow=LEGACY_WORKFLOW)
        self.assertEqual((code, err), (0, ''))
        self.assertEqual(gh.call_args.args, ('workflow', 'run', 'addon-release.yml', '--repo', cli.DEFAULT_REPO,
                                           '--ref', 'main', '-f', 'addon=codex', '-f', 'version=0.4.9'))
        self.assertIn('build main at dispatch time', out)
        gh.assert_any_call('api', f'repos/{cli.DEFAULT_REPO}/contents/.github/workflows/addon-release.yml?ref={SHA}',
                           '-H', 'Accept: application/vnd.github.raw+json')

    def test_all_stops_before_dispatch_when_existing_workflow_excludes_language_addons(self):
        code, out, err, gh = self.invoke(['--all', '--patch'], interactive=False,
                                       responses=self.batch_replies(workflow=LEGACY_WORKFLOW))
        self.assertEqual(code, 1)
        self.assertIn('does not allow addon value(s): lsp-rust', err)
        self.assertIn('addon type: string', err)
        self.assertIn('into main', err)
        self.assertIn('No workflows were dispatched', err)
        self.assert_no_dispatch(gh)
        self.assertNotIn('workflow dispatched', out)

    def test_explicit_unsupported_addon_is_rejected_before_dispatch(self):
        code, _, err, gh = self.invoke(['--addon', 'lsp-rust', '--patch'], interactive=False,
                                       addon='lsp-rust', workflow=LEGACY_WORKFLOW)
        self.assertEqual(code, 1)
        self.assertIn('does not allow addon value(s): lsp-rust', err)
        self.assert_no_dispatch(gh)

    def test_existing_workflow_can_dispatch_a_batch_of_allowed_addons(self):
        code, _, err, gh = self.invoke(['--addon', 'codex', '--addon', 'github', '--patch'], interactive=False,
                                       responses=self.batch_replies(('codex', 'github'), workflow=LEGACY_WORKFLOW))
        self.assertEqual((code, err), (0, ''))
        dispatches = [call.args for call in gh.call_args_list if call.args[:2] == ('workflow', 'run')]
        self.assertEqual(len(dispatches), 2)
        self.assertIn('addon=codex', dispatches[0])
        self.assertIn('addon=github', dispatches[1])

    def test_version_choices_are_also_validated_before_dispatch(self):
        workflow = PINNED_WORKFLOW.replace('      version:\n        type: string',
                                          '      version:\n        type: choice\n        options: [1.0.0]')
        code, _, err, gh = self.invoke(['--all', '--patch'], interactive=False,
                                       responses=self.batch_replies(workflow=workflow))
        self.assertEqual(code, 1)
        self.assertIn('does not allow version value(s)', err)
        self.assert_no_dispatch(gh)

    def test_unreadable_choices_fail_closed_before_dispatch(self):
        for options in ['[]', 'some-alias', '["unexpected,value"]']:
            workflow = LEGACY_WORKFLOW.replace('[github, codex, opencode]', options)
            code, _, _, gh = self.invoke(['--addon', 'github', '--patch'], workflow=workflow)
            self.assertEqual(code, 1)
            self.assert_no_dispatch(gh)

    def test_freeform_addons_work_without_source_pinning_support(self):
        code, out, err, gh = self.invoke(['--all', '--patch'], interactive=False,
                                       responses=self.batch_replies(workflow=FREEFORM_WORKFLOW))
        self.assertEqual((code, err), (0, ''))
        dispatches = [call.args for call in gh.call_args_list if call.args[:2] == ('workflow', 'run')]
        self.assertEqual(len(dispatches), 3)
        for call in dispatches:
            self.assertFalse(any(arg.startswith('source-sha=') for arg in call))
        self.assertEqual(out.count('build main at dispatch time'), 1)

    def test_incompatible_workflow_fails_before_any_dispatch(self):
        code, _, err, gh = self.invoke(['--addon', 'github', '--patch'], interactive=False,
                                      workflow='on:\n  workflow_call:\n    inputs:\n      source-sha:\n')
        self.assertEqual(code, 1)
        self.assertIn('required manual inputs: addon, version', err)
        self.assert_no_dispatch(gh)

    def test_all_bump_flags_release_each_addon_from_its_own_baseline(self):
        for bump, expected in [
            ('patch', ['0.8.10', '2.3.5', '0.2.1']),
            ('minor', ['0.9.0', '2.4.0', '0.3.0']),
            ('major', ['1.0.0', '3.0.0', '1.0.0']),
        ]:
            with self.subTest(bump=bump):
                code, out, err, gh = self.invoke(['--all', f'--{bump}'], interactive=False,
                                               responses=self.batch_replies())
                self.assertEqual((code, err), (0, ''))
                dispatches = [call.args for call in gh.call_args_list if call.args[:2] == ('workflow', 'run')]
                self.assertEqual(len(dispatches), 3)
                for call, addon, version in zip(dispatches, ('codex', 'github', 'lsp-rust'), expected):
                    self.assertIn(f'addon={addon}', call)
                    self.assertIn(f'version={version}', call)
                    self.assertIn(f'source-sha={SHA}', call)
                    self.assertIn(f'Tag: addon_{addon}_{version}', out)
                page_calls = [call for call in gh.call_args_list if '--paginate' in call.args]
                self.assertEqual(len(page_calls), 1)
                # All manifests must be read before any remote dispatch.
                first_dispatch = next(i for i, call in enumerate(gh.call_args_list)
                                      if call.args[:2] == ('workflow', 'run'))
                self.assertEqual(first_dispatch, 8)

    def test_bump_flags_work_for_one_addon_without_version_prompt(self):
        for bump, expected in [('patch', '0.4.9'), ('minor', '0.5.0'), ('major', '1.0.0')]:
            code, _, err, gh = self.invoke(['--addon', 'github', f'--{bump}'], interactive=False)
            self.assertEqual((code, err), (0, ''))
            self.assertIn(f'version={expected}', gh.call_args.args)
        code, out, err, gh = self.invoke(['--minor'], answers=['2'])
        self.assertEqual((code, err), (0, ''))
        self.assertIn('Select an addon', out)
        self.assertIn('version=0.5.0', gh.call_args.args)

    def test_repeated_addons_keep_order_and_duplicates_are_released_once(self):
        code, _, err, gh = self.invoke(['--addon', 'github', '--addon', 'codex', '--addon', 'github', '--patch'],
                                       interactive=False, responses=self.batch_replies(('github', 'codex')))
        self.assertEqual((code, err), (0, ''))
        dispatches = [call.args for call in gh.call_args_list if call.args[:2] == ('workflow', 'run')]
        self.assertEqual(len(dispatches), 2)
        self.assertIn('addon=github', dispatches[0])
        self.assertIn('addon=codex', dispatches[1])

    def test_conflicting_flags_fail_before_network_calls(self):
        for args in [
            ['--all', '--addon', 'github', '--patch'],
            ['--all', '--major', '--minor'],
            ['--all', '--minor', '--patch'],
            ['--all', '--major', '--patch'],
            ['--all', '--version', '1.0.0', '--patch'],
        ]:
            with patch.object(cli, 'gh') as gh, redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as error:
                    cli.main(args)
                self.assertEqual(error.exception.code, 2)
                gh.assert_not_called()

    def test_all_with_explicit_version_validates_every_addon_before_dispatch(self):
        code, _, err, gh = self.invoke(['--all', '--version', '3.0.0'], interactive=False,
                                       responses=self.batch_replies())
        self.assertEqual((code, err), (0, ''))
        dispatches = [call.args for call in gh.call_args_list if call.args[:2] == ('workflow', 'run')]
        self.assertEqual(len(dispatches), 3)
        self.assertTrue(all('version=3.0.0' in call for call in dispatches))
        code, _, err, gh = self.invoke(['--all', '--version', '1.0.0'], interactive=False,
                                       responses=self.batch_replies())
        self.assertEqual(code, 1)
        self.assertIn('Explicit version must exceed', err)
        self.assert_no_dispatch(gh)

    def test_batch_preflight_failure_or_cancel_does_not_start_any_release(self):
        code, _, err, gh = self.invoke(['--all', '--patch'], interactive=False,
                                       responses=self.batch_replies(), failure=(6, 'manifest unavailable'))
        self.assertEqual(code, 1)
        self.assertIn('manifest unavailable', err)
        self.assert_no_dispatch(gh)
        code, _, _, gh = self.invoke(['--all'], answers=['', '0'], responses=self.batch_replies())
        self.assertEqual(code, 130)
        self.assert_no_dispatch(gh)

    def test_batch_dispatch_failure_reports_partial_progress_and_stops(self):
        code, out, err, gh = self.invoke(['--all', '--patch'], interactive=False,
                                        responses=self.batch_replies(), failure=(9, 'dispatch rejected'))
        self.assertEqual(code, 1)
        self.assertIn('dispatched for codex', out)
        self.assertIn('Dispatch failed for github', err)
        self.assertIn('Already dispatched: codex', err)
        self.assertIn('Not attempted: lsp-rust', err)
        dispatches = [call.args for call in gh.call_args_list if call.args[:2] == ('workflow', 'run')]
        self.assertEqual(len(dispatches), 2)

    def test_semver_bumps_use_numeric_maximum_source_and_reserved_versions(self):
        choices = cli.version_choices('0.4.8', ['0.4.9', '0.4.10'])
        self.assertEqual([v for v, _ in choices], ['0.4.11', '0.5.0', '1.0.0'])
        self.assertEqual([v for v, _ in cli.version_choices('2.5.7', ['1.9.99'])],
                         ['2.5.8', '2.6.0', '3.0.0'])
        self.assertEqual([v for v, _ in cli.version_choices('0.0.0', [])],
                         ['0.0.1', '0.1.0', '1.0.0'])
        max_number = str(2**64 - 1)
        self.assertEqual([v for v, _ in cli.version_choices(f'1.2.{max_number}', [])],
                         ['1.3.0', '2.0.0'])

    def test_discovery_pins_remote_source_and_only_includes_addon_manifests(self):
        with patch.object(cli, 'gh', side_effect=[SHA, json.dumps(TREE)]) as gh:
            sha, addons = cli.discover('owner/repo')
        self.assertEqual(sha, SHA)
        self.assertEqual(sorted(addons), ['codex', 'github', 'lsp-rust'])
        self.assertEqual(gh.call_args_list[0].args, ('api', 'repos/owner/repo/commits/main', '--jq', '.sha'))
        self.assertEqual(gh.call_args_list[1].args, ('api', f'repos/owner/repo/git/trees/{SHA}?recursive=1'))
        for tree in [{'tree': [], 'truncated': True}, {'tree': []}]:
            with patch.object(cli, 'gh', side_effect=[SHA, json.dumps(tree)]):
                with self.assertRaises(ValueError):
                    cli.discover('owner/repo')

    def test_release_pages_include_drafts_but_exclude_other_addons_and_unstable_tags(self):
        pages = [[release('addon_github_0.4.9'), release('addon_codex_9.0.0'), release('v1.0.0')],
                 [release('addon_github_0.4.10', draft=True), release('addon_github_0.5.0-rc.1'),
                  release('addon_github_01.0.0'), release('addon_github_0.4.9')]]
        with patch.object(cli, 'gh', return_value=page_stream(pages)) as gh:
            self.assertEqual(cli.release_versions('owner/repo', 'github'), ['0.4.9', '0.4.10'])
        gh.assert_called_once_with('api', '--paginate', 'repos/owner/repo/releases?per_page=100')

    def test_explicit_arguments_dispatch_without_prompting_even_noninteractively(self):
        code, out, err, gh = self.invoke(['--addon', 'github', '--version', '0.5.0', '--repo', 'owner/repo'],
                                       interactive=False)
        self.assertEqual((code, err), (0, ''))
        gh.assert_any_call('api', f'repos/owner/repo/contents/addons/github/addon.toml?ref={SHA}',
                           '-H', 'Accept: application/vnd.github.raw+json')
        self.assertEqual(gh.call_args.args, ('workflow', 'run', 'addon-release.yml', '--repo', 'owner/repo',
                                          '--ref', 'main', '-f', 'addon=github', '-f', 'version=0.5.0',
                                          '-f', f'source-sha={SHA}'))
        self.assertIn('Tag: addon_github_0.5.0', out)
        self.assertIn('publishing has not finished', out)
        self.assertIn('https://github.com/owner/repo/actions/workflows/addon-release.yml', out)

    def test_interactive_addon_menu_and_default_patch(self):
        code, out, err, gh = self.invoke(answers=['wrong', '2', ''],
                                       releases=[[release('addon_github_0.4.9')],
                                                 [release('addon_github_0.4.10', draft=True)]])
        self.assertEqual((code, err), (0, ''))
        self.assertIn('1) codex', out)
        self.assertIn('3) lsp-rust', out)
        self.assertIn('0.4.11', out)
        self.assertIn('0.5.0', out)
        self.assertIn('1.0.0', out)
        self.assertIn('version=0.4.11', gh.call_args.args)

    def test_minor_major_custom_and_language_addon(self):
        for answer, expected in [('2', '0.5.0'), ('3', '1.0.0')]:
            code, _, _, gh = self.invoke(['--addon', 'github'], answers=[answer])
            self.assertEqual(code, 0)
            self.assertIn(f'version={expected}', gh.call_args.args)
        code, out, err, gh = self.invoke(['--addon', 'lsp-rust'],
                                       answers=['4', '', '0.1.0', '1.0.0-rc.1', '0.6.0'], addon='lsp-rust')
        self.assertEqual((code, err), (0, ''))
        self.assertIn('Enter a stable x.y.z', out)
        self.assertIn('addon=lsp-rust', gh.call_args.args)
        self.assertIn('version=0.6.0', gh.call_args.args)

    def test_missing_noninteractive_arguments_fail_before_network_calls(self):
        for args in [[], ['--addon', 'github'], ['--version', '1.0.0'], ['--all'], ['--patch']]:
            with patch.object(cli.sys.stdin, 'isatty', return_value=False), patch.object(cli, 'gh') as gh:
                with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                    cli.main(args)
                self.assertEqual(error.exception.code, 2)
                gh.assert_not_called()

    def test_cancel_eof_and_interrupt_never_dispatch(self):
        for answers in [['0'], [EOFError()], [KeyboardInterrupt()], ['4', 'q']]:
            code, _, err, gh = self.invoke(['--addon', 'github'], answers=answers)
            self.assertEqual(code, 130)
            self.assertIn('cancelled', err)
            self.assert_no_dispatch(gh)

    def test_invalid_and_reserved_versions_never_dispatch(self):
        for version in ['0.4.7', '0.4.9', '0.4.10', '1.0.0-rc.1', 'v1.0.0', '01.0.0']:
            code, _, err, gh = self.invoke(['--addon', 'github', '--version', version],
                                          releases=[[release('addon_github_0.4.10', draft=True)]])
            self.assertEqual(code, 1)
            self.assertIn('Error:', err)
            self.assert_no_dispatch(gh)
        code, _, err, gh = self.invoke(['--addon', 'unknown', '--version', '1.0.0'])
        self.assertEqual(code, 1)
        self.assertIn('Unknown addon', err)
        self.assert_no_dispatch(gh)

    def test_missing_cli_auth_and_api_failures_never_dispatch(self):
        code, _, err, gh = self.invoke(['--addon', 'github', '--version', '1.0.0'], available=False)
        self.assertEqual(code, 1)
        self.assertIn('gh auth login', err)
        gh.assert_not_called()
        for step in range(6):
            code, _, err, gh = self.invoke(['--addon', 'github', '--version', '1.0.0'],
                                          failure=(step, 'unavailable'))
            self.assertEqual(code, 1)
            self.assertIn('unavailable', err)
            self.assert_no_dispatch(gh)

    def test_dispatch_failure_is_reported_without_success_or_retry(self):
        code, out, err, gh = self.invoke(['--addon', 'github', '--version', '1.0.0'],
                                       failure=(6, 'dispatch rejected'))
        self.assertEqual(code, 1)
        self.assertIn('dispatch rejected', err)
        self.assertNotIn('workflow dispatched', out)
        self.assertEqual(sum(call.args[:2] == ('workflow', 'run') for call in gh.call_args_list), 1)

    def test_gh_wrapper_uses_argument_array_and_reports_errors(self):
        with patch.object(cli.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, ' result\n', '')) as run:
            self.assertEqual(cli.gh('api', 'repos/owner/repo/releases?per_page=100'), 'result')
            run.assert_called_once_with(['gh', 'api', 'repos/owner/repo/releases?per_page=100'],
                                        text=True, capture_output=True)
        with patch.object(cli.subprocess, 'run', return_value=subprocess.CompletedProcess([], 1, '', 'not authenticated')):
            with self.assertRaisesRegex(RuntimeError, 'not authenticated'):
                cli.gh('auth', 'status')


if __name__ == '__main__':
    unittest.main()
