"""Exercise release commits against local repositories without remote services."""
from pathlib import Path
import subprocess
import tempfile
import unittest

from commit_version import commit_version, git


def command(*args):
    subprocess.run(args, check=True, capture_output=True)


class CommitVersionTests(unittest.TestCase):
    def checkout(self, directory):
        remote = directory / "remote.git"
        root = directory / "checkout"
        command("git", "init", "--bare", str(remote))
        command("git", "init", "-b", "main", str(root))
        git(root, "config", "user.name", "Release test")
        git(root, "config", "user.email", "release@example.test")
        for project, name in ((root, "focus-editor"), (root / "addons/fixture", "focus-addon-fixture")):
            project.mkdir(parents=True, exist_ok=True)
            (project / "Cargo.toml").write_text(f'[package]\nname = "{name}"\nversion = "0.1.0"\n')
            (project / "Cargo.lock").write_text(f'version = 4\n\n[[package]]\nname = "{name}"\nversion = "0.1.0"\n')
        (root / "addons/fixture/addon.toml").write_text('id="fixture"\nversion="0.1.0"\nexecutable="program"\nicon="icon.png"\n')
        (root / "unrelated.txt").write_text("unchanged")
        git(root, "add", ".")
        git(root, "commit", "-m", "Initial")
        git(root, "remote", "add", "origin", str(remote))
        git(root, "push", "origin", "main")
        return root, remote

    def test_commit_push_repeat_and_pinned_sha(self):
        with tempfile.TemporaryDirectory() as temporary:
            root, remote = self.checkout(Path(temporary))
            sha = commit_version("v1.2.3", root)
            self.assertEqual(git(remote, "rev-parse", "main"), sha)
            self.assertEqual(git(root, "show", "-s", "--format=%s", sha), "chore: release 1.2.3")
            self.assertEqual(set(git(root, "diff-tree", "--no-commit-id", "--name-only", "-r", sha).splitlines()), {
                "Cargo.toml", "Cargo.lock", "addons/fixture/Cargo.toml", "addons/fixture/Cargo.lock", "addons/fixture/addon.toml"})
            self.assertEqual(commit_version("v1.2.3", root), sha)
            git(root, "commit", "--allow-empty", "-m", "Main advanced")
            git(root, "checkout", "--detach", sha)
            self.assertIn('version = "1.2.3"', (root / "Cargo.toml").read_text())
            with self.assertRaisesRegex(ValueError, "main checkout"):
                commit_version("v1.2.4", root)

    def test_push_rejected_when_main_advanced(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            root, remote = self.checkout(directory)
            other = directory / "other"
            command("git", "clone", "--branch", "main", str(remote), str(other))
            git(other, "config", "user.name", "Other")
            git(other, "config", "user.email", "other@example.test")
            git(other, "commit", "--allow-empty", "-m", "Concurrent update")
            git(other, "push", "origin", "main")
            remote_sha = git(remote, "rev-parse", "main")
            with self.assertRaises(subprocess.CalledProcessError):
                commit_version("v1.2.3", root)
            self.assertEqual(git(remote, "rev-parse", "main"), remote_sha)

    def test_dirty_checkout_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root, _ = self.checkout(Path(temporary))
            (root / "unrelated.txt").write_text("local change")
            with self.assertRaisesRegex(ValueError, "clean checkout"):
                commit_version("v1.2.3", root)
