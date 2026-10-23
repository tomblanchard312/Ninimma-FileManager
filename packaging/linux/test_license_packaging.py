"""Exercise shipped notices in the ZIP assembler and ordinary-user installer."""
import os
import re
import shutil
import subprocess
import tempfile
import unittest
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
APP = re.search(r'^name = "([^"]+)"', (ROOT / 'Cargo.toml').read_text(), re.M)[1]


class LicensePackagingTests(unittest.TestCase):
    def stage_notices(self, destination):
        for name in ('LICENSE', 'NOTICE'):
            shutil.copyfile(ROOT / name, destination / name)
        shutil.copyfile(ROOT / 'packaging/icons/LICENSE-lucide', destination / 'LICENSE-lucide')

    def test_user_install_retains_notices_and_quotes_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'download'
            source.mkdir()
            self.stage_notices(source)
            shutil.copyfile(ROOT / 'packaging/linux/install.sh', source / 'install.sh')
            shutil.copyfile(ROOT / f'packaging/linux/{APP}.desktop', source / f'{APP}.desktop')
            binary = source / APP
            binary.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$TEST_DEFAULT_ARGUMENTS"\n')
            binary.chmod(0o755)
            (source / 'icons').mkdir()
            for name in (f'{APP}-16.png',):
                shutil.copyfile(ROOT / 'assets/icons' / name, source / 'icons' / name)
            shutil.copyfile(ROOT / 'assets/icon.svg', source / f'icons/{APP}.svg')
            data = root / 'user data & files'
            bin_dir = root / 'user bin & files'
            arguments = root / 'default-arguments'
            env = dict(os.environ, HOME=str(root), XDG_DATA_HOME=str(data),
                       XDG_BIN_HOME=str(bin_dir), TEST_DEFAULT_ARGUMENTS=str(arguments))
            subprocess.run(['sh', str(source / 'install.sh')], env=env,
                           check=True, capture_output=True)
            self.assertFalse(arguments.exists(), 'ordinary installation must not change defaults')
            for name in ('LICENSE', 'NOTICE', 'LICENSE-lucide'):
                self.assertEqual((data / f'licenses/{APP}' / name).read_bytes(),
                                 (source / name).read_bytes())
            desktop = (data / f'applications/{APP}.desktop').read_text()
            self.assertIn(f'Exec="{bin_dir / APP}"', desktop)
            subprocess.run(['sh', str(source / 'install.sh'), '--default'], env=env,
                           check=True, capture_output=True)
            self.assertEqual(arguments.read_text(), '--make-default\n')

    @unittest.skipUnless(shutil.which('zip'), 'zip is required')
    def test_windows_zip_contains_full_lucide_and_project_notices(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.stage_notices(root)
            (root / 'packaging/icons').mkdir(parents=True)
            shutil.copyfile(root / 'LICENSE-lucide', root / 'packaging/icons/LICENSE-lucide')
            release = root / 'target/windows/release'
            release.mkdir(parents=True)
            (release / f'{APP}.exe').write_bytes(b'fixture executable')
            script = (ROOT / 'packaging/ci/build-windows-release.sh').read_text()
            assembly = script[script.index('cp LICENSE NOTICE "$release/"'):]
            subprocess.run(['bash', '-euc', 'release=target/windows/release\narchive=fixture.zip\nruntime=()\n' + assembly],
                           cwd=root, check=True, capture_output=True)
            with zipfile.ZipFile(root / 'fixture.zip') as archive:
                for name in ('LICENSE', 'NOTICE', 'LICENSE-lucide'):
                    self.assertEqual(archive.read(name), (root / name).read_bytes())
