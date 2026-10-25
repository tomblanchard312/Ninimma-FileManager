"""Prevent mutable external action references from returning to CI or releases."""
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class WorkflowSecurityTests(unittest.TestCase):
    def test_every_external_action_is_pinned_to_a_full_commit(self):
        external = []
        for path in (ROOT / '.github/workflows').iterdir():
            if path.suffix not in ('.yml', '.yaml'):
                continue
            for action in re.findall(r'uses:\s*([^\s#]+)', path.read_text()):
                if action.startswith('./'):
                    continue
                external.append(action)
                with self.subTest(workflow=path.name, action=action):
                    self.assertRegex(action, r'^[\w.-]+/[\w./-]+@[0-9a-f]{40}$')
        self.assertTrue(external, 'the test must inspect real workflow action references')
