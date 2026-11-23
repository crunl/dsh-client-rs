#!/usr/bin/env python3
"""Package a pinned dsh runtime as a self-verifying .coworkrt archive.

The archive is deliberately opaque to cowork.app.  The app only reads the
companion runtime-manifest.json, verifies the archive hash, installs it under
$DSH_HOME/profiles/<profile>/runtimes/<id>/ (default ~/.dsh/profiles/cowork/runtimes),
and launches bin/cowork-host.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
import platform
import re
import shutil
import stat
import sys
import tarfile
import tempfile
from collections import Counter
from pathlib import Path

LAUNCHER = """#!/bin/sh
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NODE="$ROOT/node/bin/node"
ENTRY="$ROOT/dsh/node_modules/@deepseek-ai/dsh/lib/bin.js"
exec "$NODE" --max-semi-space-size=4 "$ENTRY" "$@"
"""

PROFILES = ("full", "code")
PACKAGE_METADATA = {".bin", ".pnpm", ".modules.yaml", ".pnpm-workspace-state-v1.json"}
SOURCE_MAP = re.compile(r"\.(?:[cm]?[jt]s|css)\.map$")
DECLARATION = re.compile(r"\.d\.[cm]?ts$")
FS_EXT_COMPILER = re.compile(
    r"^build/(?:Release|Debug)/(?:obj(?:/|$)|fs_ext\.(?:exp|lib|pdb|iobj|ipdb)$)"
)
FS_EXT_CONFIG = re.compile(
    r"^build/(?:binding\.sln|config\.gypi|fs_ext\.vcxproj(?:\.filters)?)$"
)


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


def target_parts(target: str) -> tuple[str, str]:
    """Map our runtime target to Node's platform/arch pair."""
    mapping = {
        "darwin-arm64": ("darwin", "arm64"),
        "darwin-x64": ("darwin", "x64"),
        "linux-x64": ("linux", "x64"),
        "windows-x64": ("win32", "x64"),
    }
    if target not in mapping:
        raise SystemExit(f"unsupported target: {target}")
    return mapping[target]


def office_engine(dsh_dir: Path, target: str) -> str:
    """Select the installed LibreOfficeKit engine using the package's own resolver."""
    candidates = {
        "darwin-arm64": ("darwin-arm64",),
        "darwin-x64": ("darwin-x64",),
        "linux-x64": ("linux-x64-glibc",),
        "windows-x64": ("win32-x64",),
    }[target]
    root = dsh_dir / "node_modules/@deepseek-ai"
    for candidate in candidates:
        if (root / f"libreoffice-kit-{candidate}").is_dir():
            return candidate
    return "wasm"


def derived_runtime_id(dsh_version: str, node_version: str, target: str, profile: str) -> str:
    base = f"dsh-{dsh_version}+node-{node_version}+{target}"
    return base if profile == "full" else f"{base}+{profile}"


def validate_runtime_id(runtime_id: str) -> str:
    if not runtime_id or any(part in runtime_id for part in ("/", "\\", "..")):
        raise SystemExit(f"invalid runtime id: {runtime_id}")
    return runtime_id


def exclusion_reason(path: str, target: tuple[str, str], office: str) -> str | None:
    """Mirror official desktopRuntimeFileExclusion for one path.

    ``path`` is relative to the staged dsh tree; package-specific rules use the
    innermost node_modules container, as the official policy does.
    """
    parts = [part for part in re.split(r"[\\/]", path) if part]
    if any(part in PACKAGE_METADATA for part in parts):
        return "package-manager metadata"

    file_name = parts[-1] if parts else ""
    if SOURCE_MAP.search(file_name):
        return "source map"
    if DECLARATION.search(file_name):
        return "TypeScript declaration"
    if file_name.endswith(".tsbuildinfo"):
        return "TypeScript build cache"

    if "node_modules" in parts:
        last = len(parts) - 1 - parts[::-1].index("node_modules")
        package_parts = parts[last + 1:]
    else:
        package_parts = parts
    name_parts = 2 if package_parts and package_parts[0].startswith("@") else 1
    name = "/".join(package_parts[:name_parts])
    entry = "/".join(package_parts[name_parts:])

    if (
        name.startswith("@deepseek-ai/libreoffice-kit-")
        and name != f"@deepseek-ai/libreoffice-kit-{office}"
    ):
        return "LibreOffice other platform"
    if name == "fs-ext" and FS_EXT_COMPILER.match(entry):
        return "fs-ext compiler output"
    if name == "fs-ext" and FS_EXT_CONFIG.match(entry):
        return "fs-ext build configuration"
    if name == "@mixmark-io/domino" and (entry == "test" or entry.startswith("test/")):
        return "Domino test fixtures"
    if name == "@mixmark-io/domino" and (entry == ".yarn" or entry.startswith(".yarn/")):
        return "Domino development tooling"
    if name == "node-pty" and target[0] != "win32" and entry.startswith("third_party/conpty"):
        return "node-pty Windows ConPTY sources"
    if name == "node-pty" and entry.startswith("prebuilds/"):
        platform_name = package_parts[name_parts + 1] if len(package_parts) > name_parts + 1 else ""
        if platform_name != f"{target[0]}-{target[1]}":
            return "node-pty other platform"
        if file_name.endswith(".pdb"):
            return "node-pty debug symbols"
    if name == "@koromix/koffi-win32-x64" and entry == "win32_x64/koffi.lib":
        return "Koffi import library"
    return None


def make_exclusion_ignore(
    src: Path, target: tuple[str, str], office: str, counts: Counter[str]
):
    def ignore(dirpath: str, names: list[str]) -> set[str]:
        root = Path(dirpath)
        omitted: set[str] = set()
        for name in names:
            relative = (root / name).relative_to(src).as_posix()
            reason = exclusion_reason(relative, target, office)
            if reason is not None:
                omitted.add(name)
                counts[reason] += 1
        return omitted

    return ignore


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def copy_tree(src: Path, dst: Path, ignore=None) -> None:
    if not src.is_dir():
        raise SystemExit(f"not a directory: {src}")
    shutil.copytree(src, dst, symlinks=True, ignore=ignore)


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
    parser.add_argument("--profile", choices=PROFILES, default="full",
                        help="code applies the official conservative Desktop omissions")
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
    runtime_id = validate_runtime_id(
        args.id or derived_runtime_id(dsh_version, node_version, args.target, args.profile)
    )

    args.out_dir.mkdir(parents=True, exist_ok=True)
    archive = args.out_dir / f"{runtime_id}.coworkrt.tar.gz"
    manifest_path = args.out_dir / "runtime-manifest.json"

    exclusions: dict[str, int] = {}
    with tempfile.TemporaryDirectory(prefix="cowork-runtime-") as temp:
        stage = Path(temp) / runtime_id
        stage.mkdir()
        (stage / "bin").mkdir()
        if args.profile == "code":
            target = target_parts(args.target)
            office = office_engine(args.dsh_dir, args.target)
            counts: Counter[str] = Counter()
            copy_tree(
                args.dsh_dir,
                stage / "dsh",
                make_exclusion_ignore(args.dsh_dir, target, office, counts),
            )
            exclusions = dict(sorted(counts.items()))
        else:
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
        if args.profile == "code":
            spec["profile"] = args.profile
            spec["exclusions"] = exclusions
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
    if args.profile == "code":
        manifest["profile"] = args.profile
        manifest["exclusions"] = exclusions
    manifest_path.write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )

    print(f"runtime_id={runtime_id}")
    print(f"target={args.target}")
    print(f"profile={args.profile}")
    print(f"exclusions={json.dumps(exclusions, sort_keys=True)}")
    print(f"archive={archive}")
    print(f"manifest={manifest_path}")
    print(f"sha256={digest}")
    print(f"size={manifest['size']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
