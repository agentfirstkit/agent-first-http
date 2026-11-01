#!/usr/bin/env python3
"""Exercise image refresh, cache isolation, and failure without a daemon."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent


class ContainerRefresh(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        for name in (
            "tests/run-in-container.sh", "tests/Dockerfile.test",
            "tests/Dockerfile.test.dockerignore", "tests/container-entrypoint.sh",
            "scripts/container-runtime.sh", "container/docker/install-backends.sh",
            "container/docker/install-browser-policy.sh",
        ):
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, target)
        binary = self.root / "bin/docker"
        binary.parent.mkdir()
        binary.write_text(f"#!{sys.executable}\n" + '''import json, os, pathlib, sys
root = pathlib.Path(os.environ["FIXTURE_ROOT"])
args = sys.argv[1:]
with (root / "calls.jsonl").open("a") as trace:
    trace.write(json.dumps(args) + "\\n")
if args[:2] == ["image", "inspect"]:
    sys.exit(0 if (root / "image").exists() else 1)
if args[0] == "build":
    if os.environ.get("FAIL_BUILD") == "1": sys.exit(9)
    (root / "image").touch()
''')
        binary.chmod(0o755)
        self.environment = {
            **os.environ, "PATH": str(binary.parent) + os.pathsep + os.environ["PATH"],
            "FIXTURE_ROOT": str(self.root), "AFHTTP_TEST_CONTAINER_RUNTIME": "docker",
            "AFHTTP_TEST_CONTAINER_IMAGE": "refresh-fixture:one",
            "AFHTTP_TEST_REFRESH_IMAGE": "0", "AFHTTP_TEST_INSTALL_CAMOUFOX": "0",
        }

    def run_driver(self):
        return subprocess.run(
            ["/bin/bash" if Path("/bin/bash").is_file() else "bash",
             str(self.root / "tests/run-in-container.sh"), "true"],
            env=self.environment, capture_output=True, text=True, timeout=20,
        )

    def calls(self):
        return [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]

    def test_refresh_updates_base_and_installed_packages_without_reusing_layers(self):
        self.assertEqual(self.run_driver().returncode, 0)
        self.assertEqual(self.run_driver().returncode, 0)
        self.assertEqual(sum(call[0] == "build" for call in self.calls()), 1)
        self.environment["AFHTTP_TEST_REFRESH_IMAGE"] = "1"
        self.assertEqual(self.run_driver().returncode, 0)
        build = [call for call in self.calls() if call[0] == "build"][-1]
        self.assertIn("--pull", build)
        self.assertIn("--no-cache", build)
        self.assertIn("--rm", self.calls()[-1])

    def test_a_different_image_cannot_inherit_another_images_stamp(self):
        self.assertEqual(self.run_driver().returncode, 0)
        self.environment["AFHTTP_TEST_CONTAINER_IMAGE"] = "refresh-fixture:two"
        self.assertEqual(self.run_driver().returncode, 0)
        self.assertEqual(sum(call[0] == "build" for call in self.calls()), 2)

    def test_switching_profiles_on_one_image_invalidates_the_previous_profile(self):
        for profile in ("0", "1", "0", "1"):
            self.environment["AFHTTP_TEST_INSTALL_CAMOUFOX"] = profile
            self.assertEqual(self.run_driver().returncode, 0)
        self.assertEqual(sum(call[0] == "build" for call in self.calls()), 4)

    def test_a_changed_recipe_is_detected_even_when_its_timestamp_is_preserved(self):
        self.assertEqual(self.run_driver().returncode, 0)
        recipe = self.root / "tests/Dockerfile.test"
        before = recipe.stat()
        recipe.write_text(recipe.read_text() + "\n# changed build input\n")
        os.utime(recipe, ns=(before.st_atime_ns, before.st_mtime_ns))
        self.assertEqual(self.run_driver().returncode, 0)
        self.assertEqual(sum(call[0] == "build" for call in self.calls()), 2)

    def test_invalid_refresh_fails_before_runtime_io(self):
        self.environment["AFHTTP_TEST_REFRESH_IMAGE"] = "yes"
        self.assertEqual(self.run_driver().returncode, 2)
        self.assertFalse((self.root / "calls.jsonl").exists())

    def test_failed_build_never_runs_a_stale_image_or_writes_a_stamp(self):
        self.environment["FAIL_BUILD"] = "1"
        self.assertEqual(self.run_driver().returncode, 9)
        self.assertFalse(any(call[0] == "run" for call in self.calls()))
        self.assertFalse((self.root / "target").exists())


if __name__ == "__main__":
    unittest.main()
