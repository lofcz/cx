#!/usr/bin/env python3
"""Stamp Cargo/package protocol versions from the fork's recorded upstream base.

Fork release timestamps are display/update identities, not API compatibility
versions. Official release tags are fetched into refs/tags/openai/ by CI so fork
tags cannot accidentally select a protocol version. Never use today's newest
release to advertise capabilities for an older source checkout.
"""

import argparse
import json
from pathlib import Path
import re
import subprocess
import tomllib


def git(root: Path, *args: str) -> str:
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def resolve(root: Path) -> dict[str, str]:
    base = (root / ".codex-plus-base").read_text().strip()
    if not re.fullmatch(r"[a-f0-9]{40}", base):
        raise ValueError("Invalid .codex-plus-base revision")
    cutoff = int(git(root, "show", "-s", "--format=%ct", base))
    candidates = []
    refs = git(root, "for-each-ref", "--format=%(refname)", "refs/tags/openai/rust-v*")
    for ref in refs.splitlines():
        match = re.fullmatch(r"refs/tags/openai/rust-v(\d+)\.(\d+)\.(\d+)", ref)
        if not match:
            continue
        candidates.append((tuple(map(int, match.groups())), ref, ".".join(match.groups())))
    for _, ref, expected in sorted(candidates, reverse=True):
        revision = git(root, "rev-parse", f"{ref}^{{commit}}")
        if int(git(root, "show", "-s", "--format=%ct", revision)) > cutoff:
            continue
        # Exclude unrelated histories even if an external ref was installed.
        if subprocess.run(["git", "-C", str(root), "merge-base", base, revision], capture_output=True).returncode:
            continue
        manifest = tomllib.loads(git(root, "show", f"{revision}:codex-rs/Cargo.toml"))
        version = manifest["workspace"]["package"]["version"]
        if version != expected or version == "0.0.0":
            raise ValueError(f"Official tag {ref} does not match its Cargo version")
        return {"upstream_base": base, "tag": ref, "revision": revision, "version": version}
    raise ValueError("No official stable release predating the recorded upstream base; fetch official tags into refs/tags/openai/")


def stamp(root: Path, version: str) -> None:
    if not re.fullmatch(r"\d+\.\d+\.\d+", version) or version == "0.0.0":
        raise ValueError("A nonzero resolved semantic version is required")
    manifest_path = root / "codex-rs/Cargo.toml"
    text = manifest_path.read_text()
    old = tomllib.loads(text)["workspace"]["package"]["version"]
    if old == version:
        return
    if old != "0.0.0":
        raise ValueError(f"Refusing to replace already stamped workspace version {old}")
    pattern = r'(\[workspace\.package\][\s\S]*?^version\s*=\s*")[^"]+("[^\n]*)'
    text, count = re.subn(pattern, lambda m: m[1] + version + m[2], text, count=1, flags=re.MULTILINE)
    if count != 1:
        raise ValueError("Cannot stamp workspace version")
    lock_path = root / "codex-rs/Cargo.lock"
    lock = lock_path.read_text()
    packages = tomllib.loads(lock)["package"]
    workspace_names = {p["name"] for p in packages if "source" not in p and p["version"] == old}
    chunks = lock.split("[[package]]")
    for index, chunk in enumerate(chunks[1:], 1):
        package = tomllib.loads(chunk)
        if package["name"] in workspace_names and "source" not in package:
            chunk = re.sub(r'^version = "[^"]+"', f'version = "{version}"', chunk, count=1, flags=re.MULTILINE)
        for name in workspace_names:
            chunk = chunk.replace(f'"{name} {old}"', f'"{name} {version}"')
        chunks[index] = chunk
    manifest_path.write_text(text)
    lock_path.write_text("[[package]]".join(chunks))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--stamp", metavar="RESOLVED_VERSION")
    args = parser.parse_args()
    if args.stamp:
        stamp(args.root, args.stamp)
    else:
        print(json.dumps(resolve(args.root)))


if __name__ == "__main__":
    main()
