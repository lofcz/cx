import importlib.util
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest

MODULE = Path(__file__).resolve().parents[1] / "cx_compat_version.py"
spec = importlib.util.spec_from_file_location("cx_compat_version", MODULE)
compat = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compat)


class CompatibilityVersionTests(unittest.TestCase):
    def test_stamping_updates_workspace_packages_but_not_registry_packages(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "codex-rs").mkdir()
            manifest = root / "codex-rs/Cargo.toml"
            manifest.write_text('[workspace.package]\nedition = "2024"\nversion = "0.0.0"\n')
            lock = root / "codex-rs/Cargo.lock"
            lock.write_text('version = 4\n\n[[package]]\nname = "codex-core"\nversion = "0.0.0"\ndependencies = ["codex-login 0.0.0"]\n\n[[package]]\nname = "codex-login"\nversion = "0.0.0"\n\n[[package]]\nname = "external"\nversion = "0.0.0"\nsource = "registry+https://example.com"\n')
            compat.stamp(root, "1.2.3")
            self.assertEqual(tomllib.loads(manifest.read_text())["workspace"]["package"]["version"], "1.2.3")
            packages = tomllib.loads(lock.read_text())["package"]
            self.assertEqual([p["version"] for p in packages], ["1.2.3", "1.2.3", "0.0.0"])
            self.assertEqual(packages[0]["dependencies"], ["codex-login 1.2.3"])
            compat.stamp(root, "1.2.3")
            with self.assertRaises(ValueError):
                compat.stamp(root, "2.0.0")

    def test_resolver_uses_release_available_at_recorded_base_not_newer_or_fork_tag(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def git(*args, date="2026-01-01T00:00:00Z"):
                import os
                env = {**os.environ, "GIT_AUTHOR_NAME": "test", "GIT_AUTHOR_EMAIL": "test@example.com", "GIT_COMMITTER_NAME": "test", "GIT_COMMITTER_EMAIL": "test@example.com", "GIT_AUTHOR_DATE": date, "GIT_COMMITTER_DATE": date}
                return subprocess.check_output(["git", "-C", str(root), *args], env=env, text=True, stderr=subprocess.DEVNULL).strip()
            git("init")
            (root / "codex-rs").mkdir()
            manifest = root / "codex-rs/Cargo.toml"
            manifest.write_text('[workspace.package]\nversion = "1.2.3"\n')
            git("add", "."); git("commit", "-m", "release")
            git("tag", "openai/rust-v1.2.3")
            git("commit", "--allow-empty", "-m", "base", date="2026-01-02T00:00:00Z")
            base = git("rev-parse", "HEAD")
            manifest.write_text('[workspace.package]\nversion = "1.3.0"\n')
            git("add", "."); git("commit", "-m", "later release", date="2026-01-03T00:00:00Z")
            git("tag", "openai/rust-v1.3.0"); git("tag", "rust-v999.0.0")
            (root / ".codex-plus-base").write_text(base)
            resolved = compat.resolve(root)
            self.assertEqual(resolved["version"], "1.2.3")
            self.assertEqual(resolved["upstream_base"], base)


if __name__ == "__main__":
    unittest.main()
