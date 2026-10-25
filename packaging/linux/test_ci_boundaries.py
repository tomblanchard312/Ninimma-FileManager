"""Keep PR caches read-only and expensive packaging outside ordinary PR checks."""
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class CiBoundaryTests(unittest.TestCase):
    def test_only_default_branch_pushes_publish_build_caches(self):
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        saves = re.findall(r"      - name: Save trusted build cache\n(.*?)(?=\n      - |\Z)", workflow, re.S)
        self.assertTrue(saves)
        for save in saves:
            self.assertIn("github.event_name == 'push'", save)
            self.assertIn("github.ref == format('refs/heads/{0}', github.event.repository.default_branch)", save)
            self.assertIn('uses: actions/cache/save@', save)
            self.assertNotIn('continue-on-error', save)
        self.assertIn('uses: actions/cache/restore@', workflow)
        self.assertNotRegex(workflow, r'uses: actions/cache@')
        self.assertNotIn('pull_request_target', workflow)
        self.assertIn("github.event.pull_request.head.repo.full_name == github.repository", workflow)
        self.assertNotRegex(workflow, r'(?m)^ +~/.cargo(?:/bin|/credentials|$)')
        self.assertNotRegex(workflow, r'(?m)^ +(?:target/release|\.ci-cargo/bin)')
        self.assertIn('steps.compiler.outputs.hash', workflow)

    def test_pr_checks_keep_tests_but_do_not_build_installers(self):
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        for command in ['cargo clippy --locked --all-targets', 'cargo test --locked',
                        'cargo test --locked --no-default-features']:
            self.assertIn(command, workflow)
        self.assertNotIn('cargo build --release', workflow)
        self.assertNotIn('makensis', workflow)
        self.assertNotIn('cargo deb', workflow)

    def test_cached_llama_patch_preserves_mtimes_and_invalidates_changed_patch(self):
        import os
        import shutil
        if not shutil.which('patch'):
            self.skipTest('patch is required')
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            scripts = root / 'packaging/llama'
            scripts.mkdir(parents=True)
            shutil.copyfile(ROOT / 'packaging/llama/patch-llama.sh', scripts / 'patch-llama.sh')
            (root / 'Cargo.lock').write_text('[[package]]\nname = "llama-cpp-sys-2"\nversion = "0.1"\nsource = "registry"\nchecksum = "fixture"\n')
            source = root / 'cargo/registry/src/fixture/llama-cpp-sys-2-0.1'
            source.mkdir(parents=True)
            (source / 'Cargo.toml').write_text('[package]\nname="fixture"\nversion="0.1"\n')
            (source / 'build.rs').write_text('old\n')
            patch = scripts / 'llama-cpp-sys-2.patch'
            def set_patch(value):
                patch.write_text('--- a/build.rs\n+++ b/build.rs\n@@ -1 +1 @@\n-old\n+' + value + '\n')
            set_patch('new')
            tools = root / 'bin'
            tools.mkdir()
            cargo = tools / 'cargo'
            cargo.write_text('#!/bin/sh\nexit 0\n')
            cargo.chmod(0o755)
            env = dict(os.environ, CARGO_HOME=str(root / 'cargo'), PATH=str(tools) + os.pathsep + os.environ['PATH'])
            command = ['sh', str(scripts / 'patch-llama.sh')]
            subprocess.run(command, env=env, check=True, capture_output=True)
            patched = root / 'target/llama-cpp-sys-2-patched/build.rs'
            self.assertEqual(patched.read_text(), 'new\n')
            os.utime(patched, (1, 1))
            subprocess.run(command, env=env, check=True, capture_output=True)
            self.assertEqual(patched.stat().st_mtime, 1)
            set_patch('newer')
            subprocess.run(command, env=env, check=True, capture_output=True)
            self.assertEqual(patched.read_text(), 'newer\n')
