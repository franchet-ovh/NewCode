import unittest
import tempfile
import subprocess
from pathlib import Path
from changes import select, changed_paths


class ChangeTests(unittest.TestCase):
    def test_addon_only(self):
        self.assertEqual(select(['addons/github/src/main.rs'])[0], ['github'])

    def test_editor_only(self):
        self.assertEqual(select(['src/main.rs', 'Cargo.lock'])[0], ['editor'])

    def test_shared_consumers(self):
        self.assertEqual(select(['crates/addon-support/src/lib.rs'])[0], ['addon-support', 'codex', 'editor', 'opencode'])
        self.assertEqual(select(['crates/addon-sdk/src/lib.rs'])[0], ['addon-sdk', 'addon-support', 'codex', 'editor', 'github', 'opencode'])

    def test_docs_and_assets(self):
        self.assertEqual(select(['README.md', 'crates/addon-sdk/README.md']), ([], False))
        self.assertEqual(select(['addons/github/icon.png'])[0], ['github'])

    def test_build_inputs_fail_closed(self):
        for path in ['.github/workflows/test.yml', 'scripts/addons.py', 'rust-toolchain.toml']:
            selected, tooling = select([path])
            self.assertIn('editor', selected)
            self.assertIn('github', selected)
            self.assertTrue(tooling)

    def test_complete_push_range_includes_renames_and_deletions(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def git(*args):
                return subprocess.check_output(['git', *args], cwd=root, text=True).strip()
            git('init', '-b', 'main')
            git('config', 'user.email', 'test@example.test')
            git('config', 'user.name', 'Test')
            old = root / 'addons/github/old.rs'
            old.parent.mkdir(parents=True)
            old.write_text('fixture')
            git('add', '.')
            git('commit', '-m', 'initial')
            base = git('rev-parse', 'HEAD')
            new = root / 'addons/codex/new.rs'
            new.parent.mkdir(parents=True)
            old.rename(new)
            git('add', '.')
            git('commit', '-m', 'move')
            (root / 'src').mkdir()
            (root / 'src/main.rs').write_text('changed')
            git('add', '.')
            git('commit', '-m', 'editor')
            paths = changed_paths(base, root=root)
            self.assertEqual(set(paths), {'addons/github/old.rs', 'addons/codex/new.rs', 'src/main.rs'})
            self.assertEqual(select(paths)[0], ['codex', 'editor', 'github'])
            self.assertEqual(set(changed_paths(base, merge_base=True, root=root)), set(paths))
            with self.assertRaises(ValueError):
                changed_paths('0' * 40, root=root)
            with self.assertRaises(subprocess.CalledProcessError):
                changed_paths('missing-base', root=root)


if __name__ == '__main__':
    unittest.main()
