import hashlib
import importlib.util
import json
from pathlib import Path
import stat
import sys
import tempfile
import unittest
import zipfile
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('enki_stage', Path(__file__).with_name('stage-enki.py'))
stage = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stage)


class RuntimeTests(unittest.TestCase):
    def test_binary_only_staging_rejects_tampering_wrong_platform_and_source(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / 'enki.zip'
            files = {name: b'fixture compiled binary' for name in stage.BINARIES}
            files.update({name: b'fixture notice' for name in stage.NOTICES})
            files['libsd_enki.so'] = b'fixture compiled library'
            metadata = {'schema': 1, 'target': 'x86_64-unknown-linux-gnu', 'version': '0.2.0-preview.1',
                        'files': {name: hashlib.sha256(data).hexdigest() for name, data in files.items()}}
            files['runtime.json'] = json.dumps(metadata).encode()
            def write(entries):
                with zipfile.ZipFile(archive, 'w') as package:
                    for name, data in entries.items():
                        package.writestr(name, data)
                return hashlib.sha256(archive.read_bytes()).hexdigest()
            digest = write(files)
            dest = root / 'staged'
            stage.stage(archive, digest, metadata['target'], metadata['version'], dest)
            self.assertEqual({p.name for p in dest.iterdir()}, set(files))
            self.assertTrue((dest / 'sharedai-daemon').stat().st_mode & 0o111)
            for sha, target in [('0' * 64, metadata['target']), (digest, 'aarch64-unknown-linux-gnu')]:
                with self.assertRaises(ValueError):
                    stage.stage(archive, sha, target, metadata['version'], root / 'rejected')
            for invalid in [[], {**metadata, 'schema': True}, {**metadata, 'files': []}]:
                digest = write({**files, 'runtime.json': json.dumps(invalid).encode()})
                with self.assertRaises(ValueError):
                    stage.stage(archive, digest, metadata['target'], metadata['version'], root / 'rejected')
            for name in ['../escape', 'private-daemon.rs', 'worker.cpp']:
                with self.subTest(name=name):
                    digest = write({**files, name: b'must not ship'})
                    with self.assertRaises(ValueError):
                        stage.stage(archive, digest, metadata['target'], metadata['version'], root / 'rejected')
            incomplete = dict(files)
            incomplete.pop('BINARY-DISTRIBUTION-LICENSE.txt')
            digest = write(incomplete)
            with self.assertRaises(ValueError):
                stage.stage(archive, digest, metadata['target'], metadata['version'], root / 'rejected')
            digest = write(files)
            with zipfile.ZipFile(archive, 'w') as package:
                for name, data in files.items():
                    if name != 'libsd_enki.so':
                        package.writestr(name, data)
                info = zipfile.ZipInfo('libsd_enki.so')
                info.create_system = 3
                info.external_attr = (stat.S_IFLNK | 0o777) << 16
                package.writestr(info, b'/outside')
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            with self.assertRaises(ValueError):
                stage.stage(archive, digest, metadata['target'], metadata['version'], root / 'rejected')
            self.assertFalse((root / 'rejected').exists())


if __name__ == '__main__':
    unittest.main()
