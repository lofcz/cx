"""Run with CX_TEST_BINARY pointing at a freshly built cx binary."""
import base64
import datetime
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import unittest
from unittest.mock import patch
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def login(name):
    claims = {"email": f"{name}@example.invalid", "exp": int(time.time()) + 3600,
              "https://api.openai.com/auth": {
                  "chatgpt_user_id": "user-" + name,
                  "chatgpt_account_id": name, "chatgpt_plan_type": "pro"}}
    encoded = base64.urlsafe_b64encode(json.dumps(claims).encode()).rstrip(b"=").decode()
    token = f"e30.{encoded}.test"
    return {"auth_mode": "chatgpt", "last_refresh": datetime.datetime.now(
        datetime.timezone.utc).isoformat(), "tokens": {
            "id_token": token, "access_token": token,
            "refresh_token": "test-refresh-" + name, "account_id": name}}


@unittest.skipUnless(os.getenv("CX_TEST_BINARY"), "set CX_TEST_BINARY to the built binary")
class AccountProfiles(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.env = {**os.environ, "CODEX_HOME": temp.name, "TOKIO_WORKER_THREADS": "1"}
        for key in ("CODEX_INTERNAL_CX_ACCOUNT_NAME", "CODEX_INTERNAL_CX_ACCOUNTS_ROOT", "CODEX_ACCESS_TOKEN",
                    "CODEX_API_KEY", "OPENAI_API_KEY", "CODEX_SQLITE_HOME"):
            self.env.pop(key, None)

    def cx(self, *args, data=None, success=True, expected_stderr=None):
        proc = subprocess.run([self.env["CX_TEST_BINARY"], "--no-daemon", "-c",
                               'cli_auth_credentials_store="file"', *args],
                              input=data, capture_output=True, cwd=self.root,
                              env=self.env, timeout=60)
        self.assertEqual(proc.returncode == 0, success, proc.stderr.decode())
        if expected_stderr is not None:
            self.assertIn(expected_stderr, proc.stderr)
        return proc.stdout

    def add(self, name):
        doc = login(name)
        payload = dict(doc, agent_identity="device-only-test-value")
        self.cx("--account", name, "--import-auth", data=json.dumps(payload).encode())
        return doc

    def test_transfer_replacement_isolation_and_saved_selection(self):
        a = self.add("first")
        self.add("second")
        self.cx("accounts", "use", "first")
        exported = self.cx("--export-auth")
        self.assertEqual(json.loads(exported)["tokens"], a["tokens"])
        self.assertNotIn(b"device-only", exported)
        other = self.cx("--account", "second", "--export-auth")
        self.assertNotEqual(other, exported)
        self.cx("--account", "third", "--import-auth", data=b"\xef\xbb\xbf" + exported)
        self.assertEqual(self.cx("--account", "third", "--export-auth"), exported)
        self.cx("--import-auth", data=b"broken", success=False)
        a["tokens"]["account_id"] = "wrong"
        self.cx("--import-auth", data=json.dumps(a).encode(), success=False)
        self.assertEqual(self.cx("--export-auth"), exported)
        self.cx("--account", "../other", "--export-auth", success=False)
        self.cx("accounts", "use", "second")
        with patch.dict(self.env, {"CODEX_INTERNAL_CX_ACCOUNTS_ROOT": str(self.root),
                                   "CODEX_INTERNAL_CX_ACCOUNT_NAME": "first",
                                   "CODEX_HOME": str(self.root / "cx-accounts" / "profile-first")}):
            self.assertEqual(self.cx("--export-auth"), exported)
        with tempfile.TemporaryDirectory() as device, patch.dict(self.env, {"CODEX_HOME": device}):
            self.cx("--import-auth", data=exported)
            self.assertEqual(self.cx("--export-auth"), exported)

    def test_add_remove_and_reimport(self):
        self.cx("-c", 'forced_login_method="api"', "accounts", "add", "work",
                success=False, expected_stderr=b"ChatGPT login is disabled")
        profile = self.root / "cx-accounts" / "profile-work"
        self.assertTrue(profile.is_dir())
        self.add("work")
        self.cx("accounts", "use", "work")
        history = profile / "history.jsonl"
        history.write_text("retained history")
        with patch.dict(self.env, {"CODEX_REVOKE_TOKEN_URL_OVERRIDE": "http://127.0.0.1:1/revoke"}):
            self.cx("accounts", "remove", "work")
        self.assertFalse((profile / "auth.json").exists())
        self.assertEqual(history.read_text(), "retained history")
        self.assertEqual(self.cx("accounts", "current").strip(), b"default")
        self.cx("accounts", "use", "work", success=False)
        self.cx("--account", "work", "--import-auth", data=b"broken", success=False)
        self.cx("--account", "work", "login", "status", success=False)
        self.assertEqual([row["name"] for row in json.loads(self.cx("accounts", "list", "--json"))["accounts"]], ["default"])
        self.add("work")
        self.cx("accounts", "use", "work")
        self.assertEqual(self.cx("accounts", "current").strip(), b"work")
        for name in ("default", "auto"):
            self.cx("accounts", "remove", name, success=False)

    def test_quota_owner_cache_and_rotation(self):
        requests = []
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                if not self.path.endswith("/usage"):
                    self.send_error(404)
                    return
                name = self.headers.get("ChatGPT-Account-ID")
                requests.append(name)
                payload = {"plan_type": "pro", "account_id": name, "user_id": "user-" + name,
                           "rate_limit_reset_credits": {"available_count": 3},
                           "rate_limit": {"allowed": name == "paid", "limit_reached": name != "paid",
                                          "primary_window": {"used_percent": 20, "limit_window_seconds": 18000,
                                                             "reset_after_seconds": 60, "reset_at": int(time.time()) + 60}}}
                secondary = dict(payload["rate_limit"]["primary_window"],
                                 used_percent=45 if name == "paid" else 5,
                                 limit_window_seconds=604800)
                payload["rate_limit"]["secondary_window"] = secondary
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(json.dumps(payload).encode())

        with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                self.add("paid")
                self.add("spent")
                config = ("-c", f'chatgpt_base_url="http://127.0.0.1:{server.server_port}"')
                rows = json.loads(self.cx(*config, "accounts", "list", "--json"))
                accounts = {row["name"]: row["quota"] for row in rows["accounts"]}
                self.assertIsNone(accounts["default"])
                self.assertEqual(accounts["paid"]["remaining_percent"], 55)
                self.assertEqual(accounts["spent"]["reset_credits"], 3)
                self.assertFalse(accounts["spent"]["available"])
                self.cx("accounts", "use", "auto")
                self.assertEqual(self.cx(*config, "accounts", "current").strip(), b"paid")
                self.cx(*config, "exec", data=b"", success=False, expected_stderr=b"Using account paid")
                self.assertCountEqual(requests, ["paid", "spent"])
                self.cx("--export-auth", success=False)
            finally:
                server.shutdown()
                thread.join()


if __name__ == "__main__":
    unittest.main()
