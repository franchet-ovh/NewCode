"""Release PR preparation without network access or local checkout changes."""
import base64
import copy
from contextlib import ExitStack, redirect_stdout, redirect_stderr
import io
import json
from pathlib import Path
import unittest
from unittest.mock import patch
import tomllib

import release_addon as cli
from addon_index import load, record

ROOT = Path(__file__).resolve().parents[2]
SHA = 'a' * 40


class ReleaseAddonTests(unittest.TestCase):
    def invoke(self, args, answers=(), interactive=False, bad_file=None, failure=None, index=None):
        blobs = []
        calls = []
        registry = copy.deepcopy(load(ROOT) if index is None else index)

        def read(repo, sha, path):
            self.assertEqual(sha, SHA)
            return bad_file[1] if bad_file and path == bad_file[0] else (ROOT / path).read_text()

        def api(repo, endpoint, data=None):
            calls.append((endpoint, data))
            if failure == endpoint:
                raise RuntimeError('simulated failure')
            if endpoint.startswith('git/trees/'):
                return {'tree': [{'path': str(p.relative_to(ROOT)), 'type': 'blob'} for p in ROOT.glob('addons/*/addon.toml')]}
            if endpoint == 'git/blobs':
                blobs.append(base64.b64decode(data['content']).decode())
                return {'sha': str(len(blobs))}
            if endpoint.startswith('git/commits/'):
                return {'tree': {'sha': 'base-tree'}}
            if endpoint in ('git/trees', 'git/commits', 'git/refs'):
                return {'sha': 'new-commit' if endpoint == 'git/commits' else 'new-tree'}
            if endpoint == 'pulls':
                return {'html_url': 'https://github.com/owner/repo/pull/123'}
            raise AssertionError(endpoint)

        with ExitStack() as stack:
            stack.enter_context(patch.object(cli.shutil, 'which', return_value='/bin/gh'))
            stack.enter_context(patch.object(cli.sys.stdin, 'isatty', return_value=interactive))
            stack.enter_context(patch('builtins.input', side_effect=answers))
            stack.enter_context(patch.object(cli, 'remote', return_value=(registry, 'blob')))
            query = stack.enter_context(patch.object(cli, 'gh', side_effect=['', SHA]))
            stack.enter_context(patch.object(cli, 'contents', side_effect=read))
            stack.enter_context(patch.object(cli, 'api', side_effect=api))
            out, err = io.StringIO(), io.StringIO()
            stack.enter_context(redirect_stdout(out))
            stack.enter_context(redirect_stderr(err))
            code = cli.main(args)
        return code, out.getvalue(), err.getvalue(), calls, blobs, query

    def files(self, calls, blobs):
        tree = next(data['tree'] for endpoint, data in calls if endpoint == 'git/trees')
        return {item['path']: blobs[int(item['sha']) - 1] for item in tree}

    def test_batch_pr_contains_consistent_metadata_and_no_local_edits(self):
        before = {p: p.read_bytes() for p in [ROOT/'addons.json', *ROOT.glob('addons/*/addon.toml'), *ROOT.glob('addons/*/Cargo.toml'), *ROOT.glob('addons/*/Cargo.lock')]}
        code, out, err, calls, blobs, query = self.invoke([
            '--addon', 'github', '--addon', 'codex', '--addon', 'github', '--patch', '--to', '0.6.0'])
        self.assertEqual((code, err), (0, ''))
        files = self.files(calls, blobs)
        self.assertEqual(set(files), {'addons.json', *[f'addons/{a}/{f}' for a in ('github', 'codex') for f in ('addon.toml', 'Cargo.toml', 'Cargo.lock')]})
        for addon in ('github', 'codex'):
            manifest = tomllib.loads(files[f'addons/{addon}/addon.toml'])
            project = tomllib.loads(files[f'addons/{addon}/Cargo.toml'])['package']
            lock = tomllib.loads(files[f'addons/{addon}/Cargo.lock'])['package']
            self.assertEqual(manifest['version'], '0.5.6')
            self.assertEqual(project['version'], manifest['version'])
            self.assertEqual(next(p['version'] for p in lock if p['name'] == project['name'] and 'source' not in p), manifest['version'])
            self.assertEqual(manifest['editor_version_req'], '>=0.4.8, <0.6.0')
            item = record(json.loads(files['addons.json']), addon, '0.5.6')
            self.assertEqual((item['status'], item['packages']), ('planned', []))
        self.assertEqual(next(data['parents'] for endpoint, data in calls if endpoint == 'git/commits'), [SHA])
        self.assertEqual(sum(endpoint == 'git/refs' for endpoint, _ in calls), 1)
        self.assertEqual(next(data['base'] for endpoint, data in calls if endpoint == 'pulls'), 'main')
        self.assertIn('/pull/123', out)
        self.assertFalse(any('release' in str(call) or 'workflow' in str(call) for call in query.call_args_list))
        self.assertEqual(before, {p: p.read_bytes() for p in before})

    def test_all_and_bumps_use_indexed_independent_baselines(self):
        for flag, expected in [('--patch', '0.5.6'), ('--minor', '0.6.0'), ('--major', '1.0.0')]:
            code, _, err, calls, blobs, _ = self.invoke(['--all', flag])
            self.assertEqual((code, err), (0, ''))
            files = self.files(calls, blobs)
            self.assertEqual(len(files), 16)
            for addon in ('codex', 'github', 'lsp-rust', 'lsp-python', 'opencode'):
                self.assertEqual(tomllib.loads(files[f'addons/{addon}/addon.toml'])['version'], expected)

    def test_invalid_batch_never_creates_remote_objects(self):
        for args in [
            ['--all', '--version', '0.5.5'], ['--all', '--version', 'bad'],
            ['--addon', 'unknown', '--patch'], ['--all', '--patch', '--from', '0.6.0'],
        ]:
            code, _, _, calls, _, _ = self.invoke(args)
            self.assertEqual(code, 1)
            self.assertFalse(any(data is not None for _, data in calls))
        code, _, err, calls, _, _ = self.invoke(['--all', '--patch'], bad_file=('addons/opencode/Cargo.lock', 'version=4\n'))
        self.assertEqual(code, 1)
        self.assertIn('Expected one local package', err)
        self.assertFalse(any(data is not None for _, data in calls))

    def test_new_release_supersedes_failed_plan_without_reusing_its_version(self):
        index = load(ROOT)
        source = tomllib.loads((ROOT/'addons/github/addon.toml').read_text())
        from addon_index import entry, planned
        source['version'] = '0.6.0'
        entry(index, 'github')['versions'].append(planned(source))
        code, _, err, calls, blobs, _ = self.invoke(['--addon', 'github', '--patch'], index=index)
        self.assertEqual((code, err), (0, ''))
        updated = json.loads(self.files(calls, blobs)['addons.json'])
        self.assertEqual(record(updated, 'github', '0.6.0')['status'], 'reserved')
        self.assertEqual(record(updated, 'github', '0.6.1')['status'], 'planned')

    def test_interactive_default_patch_custom_and_cancellation(self):
        for answers, expected in [([''], '0.5.6'), (['2'], '0.6.0'), (['3'], '1.0.0'), (['4', '0.1.0', '0.7.0'], '0.7.0')]:
            code, _, err, calls, blobs, _ = self.invoke(['--addon', 'github'], answers=answers, interactive=True)
            self.assertEqual((code, err), (0, ''))
            self.assertEqual(tomllib.loads(self.files(calls, blobs)['addons/github/addon.toml'])['version'], expected)
        for answers in [['0'], [EOFError()], [KeyboardInterrupt()]]:
            code, _, _, calls, _, _ = self.invoke(['--all'], answers=answers, interactive=True)
            self.assertEqual(code, 130)
            self.assertFalse(any(data is not None for _, data in calls))

    def test_pr_failure_reports_branch_for_recovery(self):
        code, _, err, calls, _, _ = self.invoke(['--addon', 'github', '--patch'], failure='pulls')
        self.assertEqual(code, 1)
        self.assertIn('Branch created:', err)
        self.assertIn('Create a PR from release/addons-', err)
        self.assertEqual(sum(endpoint == 'git/refs' for endpoint, _ in calls), 1)

    def test_invalid_arguments_do_not_contact_github(self):
        for args in [[], ['--all'], ['--patch'], ['--all', '--addon', 'github', '--patch'], ['--all', '--patch', '--minor']]:
            with patch.object(cli, 'gh') as query, patch.object(cli.sys.stdin, 'isatty', return_value=False), redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit):
                    cli.main(args)
                query.assert_not_called()

    def test_semver_choices_use_numeric_order_and_overflow_limits(self):
        self.assertEqual([v for v, _ in cli.version_choices('0.4.8', ['0.4.9', '0.4.10'])], ['0.4.11', '0.5.0', '1.0.0'])
        self.assertEqual([v for v, _ in cli.version_choices(f'1.2.{2**64-1}', [])], ['1.3.0', '2.0.0'])


if __name__ == '__main__':
    unittest.main()
