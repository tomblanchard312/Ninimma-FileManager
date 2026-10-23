"""Check software-center device claims against the application's minimum window."""
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path


class HardwareMetadataTests(unittest.TestCase):
    def test_supported_inputs_and_screen_sizes(self):
        path = next(Path(__file__).parent.glob('*.metainfo.xml'))
        root = ET.parse(path).getroot()
        self.assertEqual({n.text for n in root.findall('supports/control')},
                         {'keyboard', 'pointing', 'touch'})
        limits = root.findall('requires/display_length')
        self.assertEqual({n.attrib['side']: int(n.text) for n in limits},
                         {'shortest': 360, 'longest': 480})
        for width, height in [(480, 360), (800, 600), (1920, 1080), (3440, 1440), (1080, 1920)]:
            for limit in limits:
                self.assertEqual(limit.attrib['compare'], 'ge')
                length = min(width, height) if limit.attrib['side'] == 'shortest' else max(width, height)
                self.assertGreaterEqual(length, int(limit.text))
