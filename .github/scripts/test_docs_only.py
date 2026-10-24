import unittest
from docs_only import docs_only


class DocsOnlyTests(unittest.TestCase):
    def test_documentation(self):
        self.assertTrue(docs_only(["README.md", "docs/user guide.md"]))

    def test_code_and_configuration_require_full_checks(self):
        for path in ("src/main.rs", "Cargo.lock", ".github/workflows/ci.yml",
                     ".github/scripts/docs_only.py", "docs/example.py", "src/help.md"):
            with self.subTest(path=path):
                self.assertFalse(docs_only(["README.md", path]))

    def test_empty_changes_fail_closed(self):
        self.assertFalse(docs_only([]))
        self.assertFalse(docs_only([""]))


if __name__ == "__main__":
    unittest.main()
