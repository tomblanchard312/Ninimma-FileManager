"""Keep installer and software-center versions aligned with the Rust package."""
import re
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
