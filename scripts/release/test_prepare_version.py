"""Release versions stay consistent without dependency resolution."""
from pathlib import Path
import shutil
import tempfile
import tomllib
import unittest
from unittest import mock

from prepare_version import prepare, release_version, validate_versions
from package import verify_binary

ROOT = Path(__file__).resolve().parents[2]


class VersionTests(unittest.TestCase):
    def test_tags(self):
        for tag in ("1.2.3", "v1.2.3", "v1.2.3-rc.1+build.2"):
            self.assertEqual(release_version(tag), tag.removeprefix("v"))
        for tag in ("", "latest", "v1", "1.2.03", "1.2.3-01", "1.2.3\n", "v18446744073709551616.0.0"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                release_version(tag)

    def test_prepare_checkout_and_locked_metadata(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            paths = [ROOT / "Cargo.toml", ROOT / "Cargo.lock"]
            paths += list((ROOT / "addons").glob("*/Cargo.toml"))
            paths += list((ROOT / "addons").glob("*/Cargo.lock"))
            paths += list((ROOT / "addons").glob("*/addon.toml"))
            for source in paths:
                destination = root / source.relative_to(ROOT)
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, destination)
            before = {p: p.read_bytes() for p in root.rglob("*") if p.is_file()}
            with self.assertRaises(ValueError):
                prepare("bad", root)
            self.assertEqual(before, {p: p.read_bytes() for p in before})
            prepare("v2.3.4-rc.1", root)
            self.assertEqual(validate_versions("v2.3.4-rc.1", root), "2.3.4-rc.1")
            with self.assertRaises(ValueError):
                validate_versions("v2.3.5", root)
            for project in [root / "Cargo.toml", *root.glob("addons/*/Cargo.toml")]:
                package = tomllib.loads(project.read_text())["package"]
                self.assertEqual(package["version"], "2.3.4-rc.1")
                lock = project.with_name("Cargo.lock")
                old = tomllib.loads(before[lock].decode())["package"]
                new = tomllib.loads(lock.read_text())["package"]
                for entry in old:
                    if entry["name"] == package["name"] and "source" not in entry:
                        entry["version"] = "2.3.4-rc.1"
                self.assertEqual(old, new)
            for manifest in root.glob("addons/*/addon.toml"):
                self.assertEqual(tomllib.loads(manifest.read_text())["version"], "2.3.4-rc.1")
            stamped = {p: p.read_bytes() for p in before}
            prepare("v2.3.4-rc.1", root)
            self.assertEqual(stamped, {p: p.read_bytes() for p in before})

    def test_binary_version_must_match(self):
        for actual in ("focus-addon-github 2.3.4", "focus-addon-github 0.1.0"):
            with mock.patch("package.run", side_effect=["ELF x86-64", "statically linked", actual]):
                if actual.endswith("2.3.4"):
                    verify_binary(Path("binary"), "x86_64-unknown-linux-gnu", "focus-addon-github", "2.3.4")
                else:
                    with self.assertRaisesRegex(RuntimeError, "version"):
                        verify_binary(Path("binary"), "x86_64-unknown-linux-gnu", "focus-addon-github", "2.3.4")
