"""Keep installer and software-center versions aligned with the Rust package."""
import re
import subprocess
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class ReleaseMetadataTests(unittest.TestCase):
    def test_release_versions_match(self):
        version = re.search(r'^version = "([^"]+)"', (ROOT / 'Cargo.toml').read_text(), re.M)[1]
        release = ET.parse(ROOT / 'packaging/linux/io.github.tomblanchard312.ninimma.metainfo.xml').find('releases/release')
        self.assertEqual(release.attrib['version'], version)
        self.assertTrue(release.findtext('url').endswith('/v' + version))
        installer = (ROOT / 'packaging/windows/ninimma.nsi').read_text()
        self.assertIn('"DisplayVersion" "' + version + '"', installer)

    def test_flatpak_packaging_ref_matches_version(self):
        version = re.search(r'^version = "([^\"]+)"', (ROOT / 'Cargo.toml').read_text(), re.M)[1]
        tag = 'v' + version
        wrong_tag = tag + '-wrong'
        for release, ref, valid in [(tag, 'refs/tags/flatpak-' + tag, True),
                                    (tag, 'refs/tags/flatpak-' + tag + '-r2', True),
                                    (tag, 'refs/tags/flatpak-' + tag + '-r0', False),
                                    (tag, 'refs/tags/flatpak-' + tag + '-r02', False),
                                    (tag, 'refs/tags/flatpak-' + tag + '-r2bad', False),
                                    (tag, 'refs/tags/' + tag, False),
                                    (tag, 'refs/heads/main', False),
                                    (tag, 'refs/tags/flatpak-' + wrong_tag, False),
                                    (wrong_tag, 'refs/tags/flatpak-' + wrong_tag, False)]:
            with self.subTest(release=release, ref=ref):
                result = subprocess.run(['bash', 'packaging/flatpak/validate-release.sh', release, ref],
                                        cwd=ROOT, capture_output=True)
                self.assertEqual(result.returncode == 0, valid)
