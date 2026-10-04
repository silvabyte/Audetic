"""Exercise the real Cargo build script without compiling the daemon or using the network.

Bun is the process boundary: the fixture models an installed dependency tree
that predates the checked-out lockfile (the source-upgrade failure).
Run with: python3 -m unittest discover -s scripts/tests -v
"""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class UiBuildTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="audetic-build-test-")
        cls.root = Path(cls.temp.name)
        cls.manifest = cls.root / "crates" / "audetic"
        cls.manifest.mkdir(parents=True)
        cls.build_script = cls.root / "build-script"
        subprocess.run(
            ["rustc", "--edition=2021", str(ROOT / "crates/audetic/build.rs"),
             "-o", str(cls.build_script)],
            env={**os.environ, "CARGO_MANIFEST_DIR": str(cls.manifest)},
            check=True,
        )

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def setUp(self):
        self.ui = self.root / "apps" / "web-ui"
        shutil.rmtree(self.ui, ignore_errors=True)
        self.ui.mkdir(parents=True)
        self.bin = self.root / "bin"
        shutil.rmtree(self.bin, ignore_errors=True)
        self.bin.mkdir()
        bun = self.bin / "bun"
        bun.write_text("""#!/bin/sh
set -eu
case "$*" in
  --version) printf '1.3.2\n' ;;
  'install --frozen-lockfile')
    printf 'install\n' >> calls
    [ "${FAIL_INSTALL:-0}" = 0 ] || exit 42
    printf 'current-lockfile\n' > installed-dependencies ;;
  'run build')
    printf 'build\n' >> calls
    if [ ! -f installed-dependencies ]; then
      printf 'Rollup failed to resolve import "markmap-view"\n' >&2
      exit 1
    fi ;;
  *) printf 'Unexpected Bun invocation: %s\n' "$*" >&2; exit 1 ;;
esac
""")
        bun.chmod(0o755)
        self.env = {**os.environ, "PATH": str(self.bin), "CARGO_CFG_TARGET_OS": "linux"}
        self.env.pop("AUDETIC_SKIP_UI_BUILD", None)

    def run_build(self, **env):
        return subprocess.run(
            [str(self.build_script)], env={**self.env, **env},
            capture_output=True, text=True,
        )

    def assert_success(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_source_upgrade_refreshes_existing_node_modules_before_build(self):
        (self.ui / "node_modules").mkdir()
        result = self.run_build()
        self.assert_success(result)
        self.assertEqual((self.ui / "calls").read_text(), "install\nbuild\n")

    def test_fresh_clone_installs_locked_dependencies(self):
        self.assert_success(self.run_build())
        self.assertEqual((self.ui / "calls").read_text(), "install\nbuild\n")

    def test_install_failure_stops_before_build_and_does_not_create_stub(self):
        (self.ui / "node_modules").mkdir()
        result = self.run_build(FAIL_INSTALL="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("bun install --frozen-lockfile", result.stderr)
        self.assertEqual((self.ui / "calls").read_text(), "install\n")
        self.assertFalse((self.ui / "dist").exists())

    def test_missing_bun_fails_with_actionable_error(self):
        (self.bin / "bun").unlink()
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Install Bun", result.stderr)
        self.assertIn("AUDETIC_SKIP_UI_BUILD=1", result.stderr)
        self.assertFalse((self.ui / "dist").exists())

    def test_explicit_skip_creates_stub_even_when_dist_is_empty(self):
        (self.ui / "dist").mkdir()
        self.assert_success(self.run_build(AUDETIC_SKIP_UI_BUILD="1"))
        self.assertIn("UI bundle not built", (self.ui / "dist/index.html").read_text())
        self.assertFalse((self.ui / "calls").exists())

    def test_explicit_skip_preserves_prebuilt_bundle(self):
        (self.ui / "dist").mkdir()
        index = self.ui / "dist/index.html"
        index.write_text("prebuilt UI")
        self.assert_success(self.run_build(AUDETIC_SKIP_UI_BUILD="1"))
        self.assertEqual(index.read_text(), "prebuilt UI")
        self.assertFalse((self.ui / "calls").exists())

    def test_zero_does_not_silently_skip_ui(self):
        self.assert_success(self.run_build(AUDETIC_SKIP_UI_BUILD="0"))
        self.assertEqual((self.ui / "calls").read_text(), "install\nbuild\n")
