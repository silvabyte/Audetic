"""Offline systemd enable/upgrade checks using the shipped service template."""

from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
TEMPLATE = ROOT / "crates/audetic/src/install/audetic.service.tmpl"


@unittest.skipUnless(sys.platform == "linux" and shutil.which("systemctl"), "requires systemd")
class LinuxServiceTests(unittest.TestCase):
    def test_upgrade_moves_autostart_from_boot_to_graphical_session(self):
        with tempfile.TemporaryDirectory(prefix="audetic-systemd-test-") as temp:
            root = Path(temp)
            units = root / "etc/systemd/user"
            units.mkdir(parents=True)
            unit = units / "audeticd.service"
            unit.write_text("[Service]\nExecStart=/bin/true\n[Install]\nWantedBy=default.target\n")

            def systemctl(verb):
                # --root + --global operates exclusively on this fixture's user
                # unit files. It never contacts or restarts the real user manager.
                result = subprocess.run(
                    ["systemctl", "--root", str(root), "--global", verb, "audeticd.service"],
                    capture_output=True, text=True,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

            systemctl("enable")
            old_link = units / "default.target.wants/audeticd.service"
            self.assertTrue(old_link.is_symlink())

            rendered = TEMPLATE.read_text()
            for name, value in {
                "__EXEC_START__": '"/bin/true"',
                "__CONFIG_DIR__": '"/tmp/config"',
                "__DATA_DIR__": '"/tmp/data"',
                "__HYPRLAND_CONFIG_DIR__": '"/tmp/hypr"',
            }.items():
                rendered = rendered.replace(name, value)
            unit.write_text(rendered)
            systemctl("reenable")

            self.assertFalse(old_link.is_symlink(), "daemon must not start before desktop login")
            self.assertTrue((units / "graphical-session.target.wants/audeticd.service").is_symlink())
            self.assertIn("PartOf=graphical-session.target", rendered, "stop at graphical logout")
            self.assertIn("After=graphical-session-pre.target", rendered)

            # Reinstallation keeps exactly the same autostart registration.
            systemctl("reenable")
            self.assertFalse(old_link.is_symlink())
            self.assertTrue((units / "graphical-session.target.wants/audeticd.service").is_symlink())
