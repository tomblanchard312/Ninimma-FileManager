# Ninimma modification: independent project packaging and branding.
"""Exercise disk lifecycles with fake tools; never attach or unlock real disks."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import disk_images as disks


class DiskTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.calls = []
        for name in ('STATE_DIR', 'MOUNT_DIR'):
            replacement = self.root / name
            replacement.mkdir()
            patcher = patch.object(disks, name, replacement)
            patcher.start()
            self.addCleanup(patcher.stop)

    def fake_run(self, *args, capture=False, check=True):
        self.calls.append(args)
        output = 'ntfs\n' if args[0] == 'blkid' else ''
        result = 1 if args[0] in ('findmnt', 'mountpoint') else 0
        return subprocess.CompletedProcess(args, result, output, '')

    def test_readonly_and_writable_layers_agree(self):
        for readonly in (True, False):
            self.calls.clear()
            state_file = disks.STATE_DIR / ('ro.json' if readonly else 'rw.json')
            with patch.object(disks, 'run', side_effect=self.fake_run), patch.object(disks, 'require'):
                state = {'uid': 1000, 'target': '/dev/sdb1'}
                disks.mount_volume(state_file, state, '/dev/sdb1', readonly, encrypted=True)
                opening = next(c for c in self.calls if c[:2] == ('cryptsetup', 'open'))
                self.assertEqual('--readonly' in opening, readonly)
                mounting = next(c for c in self.calls if c[0] == 'mount')
                self.assertIn('ro' if readonly else 'rw', mounting[2].split(','))
                self.assertIn('uid=1000', mounting[2])
                disks.cleanup(state_file, state)

    def test_busy_mount_preserves_mapping_and_image(self):
        state_file = disks.STATE_DIR / 'busy.json'
        state = {'uid': 1000, 'mount': '/mnt/example', 'mapper': 'example', 'nbd': '/dev/nbd0'}
        disks.save(state_file, state)

        def busy(*args, **kwargs):
            self.calls.append(args)
            if args[0] == 'umount':
                raise subprocess.CalledProcessError(1, args)
            return subprocess.CompletedProcess(args, 0, '', '')

        with patch.object(disks, 'run', side_effect=busy):
            with self.assertRaises(subprocess.CalledProcessError):
                disks.cleanup(state_file, state)
        self.assertEqual(json.loads(state_file.read_text()), state)
        self.assertFalse(any(c[0] in ('cryptsetup', 'qemu-nbd') for c in self.calls))

    def test_cancelled_partition_releases_nbd_and_state(self):
        image = self.root / "disk ' with spaces.vhdx"
        image.touch()
        with patch.object(disks.os, 'geteuid', return_value=0), \
                patch.object(disks, 'secure_directory'), \
                patch.object(disks, 'require'), \
                patch.object(disks, 'free_nbd', return_value='/dev/nbd0'), \
                patch.object(disks, 'run', side_effect=self.fake_run), \
                patch.object(disks, 'select_partition', side_effect=RuntimeError('Cancelled')):
            with self.assertRaisesRegex(RuntimeError, 'Cancelled'):
                disks.main('mount', str(image), 'ro')
        attach = next(c for c in self.calls if '--connect=/dev/nbd0' in c)
        self.assertIn('--read-only', attach)
        self.assertEqual(attach[-1], str(image))
        self.assertIn(('qemu-nbd', '--disconnect', '/dev/nbd0'), self.calls)
        self.assertFalse(list(disks.STATE_DIR.glob('*.json')))

    def test_wrong_password_releases_attached_image(self):
        state_file = disks.STATE_DIR / 'password.json'
        state = {'uid': 1000, 'nbd': '/dev/nbd0'}
        disks.save(state_file, state)

        def reject(*args, **kwargs):
            if args[:2] == ('cryptsetup', 'open'):
                raise subprocess.CalledProcessError(2, args)
            return self.fake_run(*args, **kwargs)

        with patch.object(disks, 'run', side_effect=reject), patch.object(disks, 'require'):
            with self.assertRaises(subprocess.CalledProcessError):
                disks.mount_volume(state_file, state, '/dev/nbd0p1', True, encrypted=True)
            disks.cleanup(state_file, state)
        self.assertNotIn('mapper', state)
        self.assertIn(('qemu-nbd', '--disconnect', '/dev/nbd0'), self.calls)

    def test_mount_failure_closes_mapping(self):
        state_file = disks.STATE_DIR / 'failed.json'
        state = {'uid': 1000}

        def fail_mount(*args, **kwargs):
            if args[0] == 'mount':
                raise subprocess.CalledProcessError(32, args)
            return self.fake_run(*args, **kwargs)

        with patch.object(disks, 'run', side_effect=fail_mount), patch.object(disks, 'require'):
            with self.assertRaises(subprocess.CalledProcessError):
                disks.mount_volume(state_file, state, '/dev/sdb1', True, encrypted=True)
            mapper = state['mapper']
            disks.cleanup(state_file, state)
        self.assertIn(('cryptsetup', 'close', mapper), self.calls)
        self.assertFalse(state_file.exists())


if __name__ == '__main__':
    unittest.main()
