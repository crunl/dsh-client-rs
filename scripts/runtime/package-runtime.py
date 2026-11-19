#!/usr/bin/env python3
"""Package a pinned dsh runtime as a self-verifying .coworkrt archive.

The archive is deliberately opaque to cowork.app.  The app only reads the
companion runtime-manifest.json, verifies the archive hash, installs it under
~/.cowork/runtimes/<id>/, and launches bin/cowork-host.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
import platform
import shutil
import stat
import sys
import tarfile
import tempfile
from pathlib import Path

LAUNCHER = """#!/bin/sh
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NODE="$ROOT/node/bin/node"
ENTRY="$ROOT/dsh/node_modules/@deepseek-ai/dsh/lib/bin.js"
exec "$NODE" --max-semi-space-size=4 "$ENTRY" "$@"
"""


def target_triple() -> str:
    system = platform.system().lower()
    machine = platform.machine().lower()
    if system == "darwin" and machine in {"arm64", "aarch64"}:
        return "darwin-arm64"
    if system == "darwin" and machine in {"x86_64", "amd64"}:
        return "darwin-x64"
    if system == "linux" and machine in {"x86_64", "amd64"}:
        return "linux-x64"
    if system == "windows" and machine in {"x86_64", "amd64"}:
        return "windows-x64"
    raise SystemExit(f"unsupported target: {system}/{machine}")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def copy_tree(src: Path, dst: Path) -> None:
    if not src.is_dir():
        raise SystemExit(f"not a directory: {src}")
    shutil.copytree(src, dst, symlinks=True)


def normalize_tar(info: tarfile.TarInfo) -> tarfile.TarInfo:
    info.mtime = 0
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    return info


def create_archive(stage: Path, archive: Path) -> None:
    with (
        archive.open("wb") as raw,
        gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as zipped,
        tarfile.open(fileobj=zipped, mode="w") as tar,
    ):
        for entry in sorted(stage.iterdir(), key=lambda p: p.name):
            tar.add(entry, arcname=entry.name, filter=normalize_tar)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dsh-dir", required=True, type=Path,
                        help="directory containing node_modules/@deepseek-ai/dsh")
    parser.add_argument("--node-dir", required=True, type=Path,
                        help="extracted official Node distribution containing bin/node")
    parser.add_argument("--dsh-version", required=True)
    parser.add_argument("--node-version", required=True)
    parser.add_argument("--out-dir", required=True, type=Path)
    parser.add_argument("--target", default=target_triple())
    parser.add_argument("--id", help="runtime id; derived from versions when omitted")
    args = parser.parse_args()

    dsh_entry = args.dsh_dir / "node_modules/@deepseek-ai/dsh/lib/bin.js"
    node_binary = args.node_dir / "bin/node"
    if not dsh_entry.is_file():
        raise SystemExit(f"missing dsh entry: {dsh_entry}")
    if not node_binary.is_file():
        raise SystemExit(f"missing node binary: {node_binary}")

    dsh_version = args.dsh_version.removeprefix("v")
    node_version = args.node_version.removeprefix("v")
    runtime_id = args.id or f"dsh-{dsh_version}+node-{node_version}+{args.target}"
    if "/" in runtime_id or ".." in runtime_id:
        raise SystemExit(f"invalid runtime id: {runtime_id}")

    args.out_dir.mkdir(parents=True, exist_ok=True)
    archive = args.out_dir / f"{runtime_id}.coworkrt.tar.gz"
    manifest_path = args.out_dir / "runtime-manifest.json"

    with tempfile.TemporaryDirectory(prefix="cowork-runtime-") as temp:
        stage = Path(temp) / runtime_id
        stage.mkdir()
        (stage / "bin").mkdir()
        copy_tree(args.dsh_dir, stage / "dsh")
        copy_tree(args.node_dir, stage / "node")

        launcher = stage / "bin/cowork-host"
        launcher.write_text(LAUNCHER, encoding="utf-8")
        launcher.chmod(launcher.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

        spec = {
            "schema": 1,
            "id": runtime_id,
            "target": args.target,
            "dsh": dsh_version,
            "node": node_version,
            "launcher": "bin/cowork-host",
            "entry": "dsh/node_modules/@deepseek-ai/dsh/lib/bin.js",
        }
        (stage / "runtime.json").write_text(
            json.dumps(spec, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )

        create_archive(stage, archive)

    digest = sha256(archive)
    manifest = {
        "schema": 1,
        "id": runtime_id,
        "target": args.target,
        "dsh": dsh_version,
        "node": node_version,
        "launcher": "bin/cowork-host",
        "archive": archive.name,
        "sha256": digest,
        "size": archive.stat().st_size,
        "url": os.environ.get("COWORK_RUNTIME_URL", ""),
    }
    manifest_path.write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )

    print(f"runtime_id={runtime_id}")
    print(f"target={args.target}")
    print(f"archive={archive}")
    print(f"manifest={manifest_path}")
    print(f"sha256={digest}")
    print(f"size={manifest['size']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
