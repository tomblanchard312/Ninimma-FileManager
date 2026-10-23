# Ninimma modification: verify a tarball install without touching real user settings.
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class InstallTests(unittest.TestCase):
    def test_per_user_install_and_desktop_actions_with_spaces(self):
        with tempfile.TemporaryDirectory(prefix="ninimma install ") as folder:
            base = Path(folder)
            payload = base / "payload"
            payload.mkdir()
            binary = payload / "ninimma"
            binary.write_text("#!/bin/sh\nexit 0\n")
            binary.chmod(0o755)
            for name in ("install.sh", "ninimma.desktop", "io.github.tomblanchard312.ninimma.metainfo.xml"):
                shutil.copy(ROOT / "packaging/linux" / name, payload / name)
            for name in ("LICENSE", "NOTICE"):
                shutil.copy(ROOT / name, payload / name)
            shutil.copy(ROOT / "packaging/icons/LICENSE-lucide", payload)
            (payload / "icons").mkdir()
            for icon in (ROOT / "assets/icons").glob("ninimma-*.png"):
                shutil.copy(icon, payload / "icons")
            shutil.copy(ROOT / "assets/icon.svg", payload / "icons/ninimma.svg")
            env = dict(os.environ, XDG_BIN_HOME=str(base / "my bin"), XDG_DATA_HOME=str(base / "my data"))
            subprocess.run(["sh", str(payload / "install.sh")], env=env, check=True, capture_output=True)
            desktop = base / "my data/applications/ninimma.desktop"
            text = desktop.read_text()
            expected = f'Exec="{base}/my bin/ninimma"'
            self.assertIn(expected + " %F", text)
            self.assertIn(expected + " --make-default", text)
            self.assertIn(expected + " --restore-default", text)
            self.assertTrue((base / "my bin/ninimma").is_file())
            for notice in ("LICENSE", "NOTICE", "LICENSE-lucide"):
                self.assertTrue((base / "my data/licenses/ninimma" / notice).is_file())
            self.assertTrue((base / "my data/icons/hicolor/256x256/apps/ninimma.png").is_file())
            if shutil.which("desktop-file-validate"):
                subprocess.run(["desktop-file-validate", str(desktop)], check=True)
