import functools
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import threading
import unittest

REPO = Path(__file__).resolve().parents[2]


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_args):
        pass


class CxInstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.web = self.root / "web"
        self.web.mkdir()
        self.server = http.server.ThreadingHTTPServer(
            ("127.0.0.1", 0), functools.partial(QuietHandler, directory=str(self.web))
        )
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.home = self.root / "home"
        self.bin = self.home / ".local/bin"
        self.bin.mkdir(parents=True)
        (self.bin / "codex").write_text("official codex remains here")
        self.config = self.home / ".codex"
        self.config.mkdir()
        (self.config / "config.toml").write_text('model = "keep-me"\n')
        self.env = {
            **os.environ,
            "HOME": str(self.home),
            "CODEX_HOME": str(self.config),
            "XDG_DATA_HOME": str(self.home / ".local/share"),
            "CX_INSTALL_DIR": str(self.bin),
            "CX_RELEASE_BASE_URL": f"http://127.0.0.1:{self.server.server_port}",
        }

    def release(self, version, corrupt=False):
        directory = self.web / "releases" / version
        directory.mkdir(parents=True)
        package = self.root / f"package-{version}"
        for name in ("bin/codex++", "bin/codex-code-mode-host", "bin/codex-exec-server", "codex-path/rg", "codex-resources/bwrap"):
            path = package / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(f"#!/bin/sh\necho 'cx {version}'\n")
            path.chmod(0o755)
        (package / "codex-package.json").write_text("{}")
        archive = directory / "codex-package-x86_64-unknown-linux-musl.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            tar.add(package, arcname=".")
        checksum = "0" * 64 if corrupt else hashlib.sha256(archive.read_bytes()).hexdigest()
        (directory / "codex-package_SHA256SUMS").write_text(f"{checksum}  {archive.name}\n")
        for name in ("install-fork.sh", "cx-local.sh"):
            shutil.copy2(REPO / "scripts/install" / name, directory / name)
        (self.web / "latest.json").write_text(json.dumps({"tag_name": f"rust-v{version}"}))

    def install(self, *args):
        return subprocess.run(["bash", str(REPO / "scripts/install/install-cx.sh"), *args], env=self.env, text=True, capture_output=True)

    def current_version(self):
        return subprocess.check_output([str(self.bin / "cx"), "--version"], env=self.env, text=True).strip()

    def test_install_and_upgrade_preserve_codex_and_config(self):
        for version in ("202611060001", "202611070001"):
            self.release(version)
            result = self.install()
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(self.current_version(), f"cx {version}")
        self.assertEqual((self.bin / "codex").read_text(), "official codex remains here")
        self.assertEqual((self.config / "config.toml").read_text(), 'model = "keep-me"\n')
        self.assertFalse((self.config / "packages").exists())

    def test_bad_checksum_keeps_last_working_release(self):
        self.release("202611060001")
        self.assertEqual(self.install().returncode, 0)
        self.release("202611070001", corrupt=True)
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Checksum mismatch", result.stderr)
        self.assertEqual(self.current_version(), "cx 202611060001")

    def test_invalid_version_does_not_install(self):
        result = self.install("../../outside")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.bin / "cx").exists())


if __name__ == "__main__":
    unittest.main()
