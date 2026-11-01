#!/usr/bin/env python3
"""Exercise the real pin checker against offline upstream release fixtures."""
import json
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
KASM_URL = "https://api.github.com/repos/kasmtech/KasmVNC/releases/latest"


class ContainerPins(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        directory = Path(self.temp.name)
        (directory / "curl").symlink_to(ROOT / "tests/support/pins-curl.py")
        self.fixture_path = directory / "upstreams.json"
        self.environment = {
            **os.environ,
            "PATH": str(directory) + os.pathsep + os.environ["PATH"],
            "AFHTTP_PIN_FIXTURES_PATH": str(self.fixture_path),
        }
        pins = dict(re.findall(r'^(\w+)="([^"]+)"$', (ROOT / "container/docker/install-backends.sh").read_text(), re.MULTILINE))
        self.version = pins["KASMVNC_VERSION"]
        self.fixtures = {
            "https://api.github.com/repos/adryfish/fingerprint-chromium/releases/latest": {
                "tag_name": pins["FINGERPRINT_CHROMIUM_VERSION"],
                "assets": [{"name": f'ungoogled-chromium-{pins["FINGERPRINT_CHROMIUM_VERSION"]}-1-x86_64_linux.tar.xz'}],
            },
            "https://api.github.com/repos/daijro/camoufox/releases/latest": {
                "tag_name": pins["CAMOUFOX_RELEASE"],
                "assets": [{"name": f'camoufox-{pins["CAMOUFOX_X86_BUILD"]}-lin.x86_64.zip'}, {"name": f'camoufox-{pins["CAMOUFOX_ARM_BUILD"]}-lin.arm64.zip'}],
            },
            "https://go.dev/dl/?mode=json": [{"version": "go" + pins["GO_VERSION"]}],
            KASM_URL: {
                "tag_name": "v" + self.version,
                "assets": [{"name": f"kasmvncserver_debian_trixie_{self.version}_{arch}.deb"} for arch in ("amd64", "arm64")],
            },
        }

    def check(self):
        self.fixture_path.write_text(json.dumps(self.fixtures))
        result = subprocess.run(["bash", str(ROOT / "tests/check_container_third_party_versions.sh")], env=self.environment, capture_output=True, text=True, timeout=20)
        self.assertNotIn("unreachable:", result.stderr)
        self.assertNotIn("unreadable:", result.stderr)
        return result

    def test_current_distribution_qualified_assets_pass(self):
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_each_supported_architecture_is_required(self):
        self.fixtures[KASM_URL]["assets"].pop()
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"missing expected KasmVNC trixie asset: kasmvncserver_debian_trixie_{self.version}_arm64.deb", result.stderr)

    def test_renamed_assets_do_not_weaken_version_drift_detection(self):
        self.fixtures[KASM_URL]["tag_name"] = "v999.0.0"
        self.fixtures[KASM_URL]["assets"] = [{"name": f"kasmvncserver_debian_trixie_999.0.0_{arch}.deb"} for arch in ("amd64", "arm64")]
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"outdated: kasmvnc current={self.version} latest=999.0.0", result.stderr)


if __name__ == "__main__":
    unittest.main()
