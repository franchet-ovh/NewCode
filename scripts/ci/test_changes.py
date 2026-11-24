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
        # Keep the dependency graph explicit so new repository addons do not
        # change this test's expectations.
        manifests = {
            'Cargo.toml': {'sdk': 'crates/addon-sdk', 'support': 'crates/addon-support'},
            'crates/addon-sdk/Cargo.toml': {},
            'crates/addon-support/Cargo.toml': {'sdk': '../addon-sdk'},
            'crates/language-addon/Cargo.toml': {'sdk': '../addon-sdk'},
            'addons/codex/Cargo.toml': {'support': '../../crates/addon-support'},
            'addons/opencode/Cargo.toml': {'support': '../../crates/addon-support'},
            'addons/github/Cargo.toml': {'sdk': '../../crates/addon-sdk'},
            'addons/lsp-python/Cargo.toml': {'language': '../../crates/language-addon'},
            'addons/lsp-rust/Cargo.toml': {'language': '../../crates/language-addon'},
            'addons/unrelated/Cargo.toml': {},
        }
        cases = {
            'addon-support': ['addon-support', 'codex', 'editor', 'opencode'],
            'addon-sdk': ['addon-sdk', 'addon-support', 'codex', 'editor', 'github',
                          'language-addon', 'lsp-python', 'lsp-rust', 'opencode'],
            'language-addon': ['language-addon', 'lsp-python', 'lsp-rust'],
        }
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative, dependencies in manifests.items():
                manifest = root / relative
                manifest.parent.mkdir(parents=True, exist_ok=True)
                manifest.write_text(
                    '[package]\nname="fixture"\nversion="0.1.0"\n\n[dependencies]\n'
                    + ''.join(f'{name} = {{ path = "{path}" }}\n'
                              for name, path in dependencies.items())
                )
            for crate, expected in cases.items():
                with self.subTest(crate=crate):
                    selected, tooling = select([f'crates/{crate}/src/lib.rs'], root=root)
                    self.assertEqual(selected, expected)
                    self.assertFalse(tooling)

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
