"""Exercise the shell installer with local releases and no network or real installs."""
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'install-release.sh'
REPOSITORY = 'https://github.com/franchet-ovh/NewCode'


@unittest.skipUnless(shutil.which('jq'), 'installer tests require jq')
class InstallReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.home = self.root / 'home with spaces'
        self.home.mkdir()
        self.bin = self.root / 'commands'
        self.bin.mkdir()
        self.fixtures = self.root / 'fixtures'
        self.fixtures.mkdir()
        self.destination = self.home / '.local/share/focus-editor/bin'
        self.env = dict(os.environ, HOME=str(self.home), PATH=f'{self.bin}:{os.environ["PATH"]}',
                        INSTALL_TEST_FIXTURES=str(self.fixtures), INSTALL_TEST_ARCH='x86_64',
                        INSTALL_TEST_OS='Linux', INSTALL_TEST_REQUESTS=str(self.root / 'requests.jsonl'))
        self.command('uname', '#!/bin/sh\ncase "$1" in -s) echo "$INSTALL_TEST_OS";; -m) echo "$INSTALL_TEST_ARCH";; esac\n')
        self.command('curl', f'''#!{shutil.which('python3')}
import json, os, pathlib, shutil, sys
args = sys.argv[1:]
url = args[-1]
with open(os.environ['INSTALL_TEST_REQUESTS'], 'a') as log:
    log.write(json.dumps(url) + '\\n')
name = 'release.json' if url.endswith('/releases/latest') else ('releases.json' if '/releases?' in url else url.rsplit('/', 1)[-1])
shutil.copyfile(pathlib.Path(os.environ['INSTALL_TEST_FIXTURES']) / name, args[args.index('--output') + 1])
''')
        self.tag = 'v1.2.3'
        self.release = {'tag_name': self.tag, 'assets': []}
        for platform in ('linux-amd64', 'linux-arm64'):
            name = f'focus-editor-{self.tag}-{platform}.tar.gz'
            with tarfile.open(self.fixtures / name, 'w:gz') as archive:
                for binary in ('focus-editor', 'focus-editor-server'):
                    self.member(archive, binary, f'{binary} {platform}'.encode())
                self.member(archive, 'addons/codex/program', b'must not install')
                self.member(archive, 'install-addons.py', b'raise RuntimeError("must not run")')
            self.asset(name)
        self.asset('SHA256SUMS')
        self.write_release()
        self.checksums()

    def command(self, name, content):
        path = self.bin / name
        path.write_text(content)
        path.chmod(0o755)

    @staticmethod
    def member(archive, name, content):
        info = tarfile.TarInfo(name)
        info.size = len(content)
        info.mode = 0o755
        archive.addfile(info, io.BytesIO(content))

    def asset(self, name):
        self.release['assets'].append({'name': name, 'browser_download_url': f'{REPOSITORY}/releases/download/{self.tag}/{name}'})

    def write_release(self):
        (self.fixtures / 'release.json').write_text(json.dumps(self.release))

    def checksums(self):
        (self.fixtures / 'SHA256SUMS').write_text(''.join(
            f'{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n'
            for path in sorted(self.fixtures.glob('*.tar.gz'))))

    def run_installer(self):
        return subprocess.run(['bash', str(SCRIPT)], env=self.env, capture_output=True, text=True, timeout=10)

    def test_addon_latest_release_falls_back_to_editor(self):
        (self.fixtures / 'releases.json').write_text(json.dumps([
            {'tag_name': 'addon_github_9.0.0', 'assets': []}, self.release]))
        (self.fixtures / 'release.json').write_text(json.dumps({'tag_name': 'addon_github_9.0.0', 'assets': []}))
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.destination / 'focus-editor').is_file())

    def seed_installation(self):
        self.destination.mkdir(parents=True)
        for binary in ('focus-editor', 'focus-editor-server'):
            (self.destination / binary).write_text('previous version')
        (self.destination / 'keep.txt').write_text('keep this file')
        addons = self.home / '.local/share/focus-editor/addons'
        addons.mkdir()
        (addons / 'existing-addon').write_text('existing data')

    def test_architecture_selection_installs_only_binaries_and_preserves_other_files(self):
        self.seed_installation()
        for arch, platform in [('x86_64', 'linux-amd64'), ('aarch64', 'linux-arm64'), ('arm64', 'linux-arm64')]:
            with self.subTest(arch=arch):
                self.env['INSTALL_TEST_ARCH'] = arch
                result = self.run_installer()
                self.assertEqual(result.returncode, 0, result.stderr)
                for binary in ('focus-editor', 'focus-editor-server'):
                    path = self.destination / binary
                    self.assertEqual(path.read_text(), f'{binary} {platform}')
                    self.assertEqual(path.stat().st_mode & 0o777, 0o755)
                self.assertEqual({p.name for p in self.destination.iterdir()}, {'focus-editor', 'focus-editor-server', 'keep.txt'})
                self.assertEqual((self.destination / 'keep.txt').read_text(), 'keep this file')
                self.assertEqual((self.destination.parent / 'addons/existing-addon').read_text(), 'existing data')
        urls = [json.loads(line) for line in (self.root / 'requests.jsonl').read_text().splitlines()]
        self.assertTrue(all('focus-addon-' not in url and 'addons.json' not in url for url in urls))

    def test_checksum_failure_preserves_previous_installation(self):
        self.seed_installation()
        (self.fixtures / f'focus-editor-{self.tag}-linux-amd64.tar.gz').write_bytes(b'tampered')
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('checksum verification failed', result.stderr)
        self.assertEqual((self.destination / 'focus-editor').read_text(), 'previous version')
        self.assertEqual((self.destination / 'focus-editor-server').read_text(), 'previous version')

    def test_unfinished_latest_release_fails_without_falling_back_or_creating_destination(self):
        self.release['assets'] = []
        self.write_release()
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('release build may still be running', result.stderr)
        self.assertFalse(self.destination.exists())
        urls = (self.root / 'requests.jsonl').read_text().splitlines()
        self.assertEqual(len(urls), 1)

    def test_missing_or_symlinked_binary_does_not_replace_existing_installation(self):
        self.seed_installation()
        path = self.fixtures / f'focus-editor-{self.tag}-linux-amd64.tar.gz'
        for symlink in (False, True):
            with self.subTest(symlink=symlink):
                with tarfile.open(path, 'w:gz') as archive:
                    self.member(archive, 'focus-editor', b'new editor')
                    if symlink:
                        info = tarfile.TarInfo('focus-editor-server')
                        info.type = tarfile.SYMTYPE
                        info.linkname = 'focus-editor'
                        archive.addfile(info)
                self.checksums()
                result = self.run_installer()
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual((self.destination / 'focus-editor').read_text(), 'previous version')
                self.assertEqual((self.destination / 'focus-editor-server').read_text(), 'previous version')

    def test_unsupported_platform_fails_before_downloading(self):
        for system, arch in [('Darwin', 'arm64'), ('Linux', 'armv7l')]:
            with self.subTest(system=system, arch=arch):
                self.env['INSTALL_TEST_OS'] = system
                self.env['INSTALL_TEST_ARCH'] = arch
                result = self.run_installer()
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((self.root / 'requests.jsonl').exists())

    def test_release_tag_uses_the_packagers_filename_sanitization(self):
        previous = self.fixtures / f'focus-editor-{self.tag}-linux-amd64.tar.gz'
        self.tag = 'release/v1.2.3+build.4'
        name = 'focus-editor-release-v1.2.3-build.4-linux-amd64.tar.gz'
        shutil.copyfile(previous, self.fixtures / name)
        self.release = {'tag_name': self.tag, 'assets': []}
        self.asset(name)
        self.asset('SHA256SUMS')
        self.write_release()
        self.checksums()
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
