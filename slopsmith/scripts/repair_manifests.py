#!/usr/bin/env python3
"""One-off repair of sloppak manifest.yaml metadata (GBK-as-BIG5 mojibake,
swapped title/artist, junk titles) directly on disk.

Usage:
    python3 scripts/repair_manifests.py [DLC_DIR] [--dry-run]

DLC_DIR resolves from argv[1], then $DLC_DIR, then the dlc_dir key in
CONFIG_DIR/config.json (same precedence as server._get_dlc_dir).

For every directory-form *.sloppak, repairs title/artist/album using
lib.meta_repair and writes the manifest back with yaml.safe_dump. A
manifest.yaml.bak backup is kept the first time a file changes (existing
.bak files are never overwritten). Junk-title sloppaks (s / 2ss / gtp.cn
stems) cannot be repaired and are skipped — the library hides them at scan
time instead.

After running, the changed mtimes make the next periodic rescan (or the
Rescan button in the app) re-extract the cleaned metadata. Run with
--dry-run first to preview.
"""

import argparse
import json
import os
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import yaml  # noqa: E402

from lib import meta_repair  # noqa: E402

CONFIG_DIR = Path(os.environ.get("CONFIG_DIR", str(Path.home() / ".local" / "share" / "rocksmith-cdlc")))


def resolve_dlc_dir() -> Path:
    env = os.environ.get("DLC_DIR", "").strip()
    if env:
        return Path(env)
    try:
        cfg = json.loads((CONFIG_DIR / "config.json").read_text(encoding="utf-8"))
        if cfg.get("dlc_dir"):
            return Path(cfg["dlc_dir"])
    except (OSError, json.JSONDecodeError):
        pass
    return Path("")


def iter_sloppaks(root: Path):
    for entry in sorted(root.iterdir()):
        if not entry.name.lower().endswith(".sloppak"):
            continue
        if entry.is_dir():
            yield entry, entry / "manifest.yaml"
        else:
            yield entry, None  # zip-form: flagged for the caller


def load_manifest(path: Path) -> dict:
    with open(path, encoding="utf-8") as fh:
        return yaml.safe_load(fh) or {}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dlc_dir", nargs="?", default="")
    parser.add_argument("--dry-run", action="store_true", help="print planned changes only")
    args = parser.parse_args()

    root = Path(args.dlc_dir) if args.dlc_dir else resolve_dlc_dir()
    if not root or not root.is_dir():
        print("ERROR: no DLC dir — pass it as an argument or set DLC_DIR / config.json dlc_dir.", file=sys.stderr)
        return 1

    changed = 0
    skipped_zip = 0
    skipped_junk = 0
    unchanged = 0

    for entry, manifest_path in iter_sloppaks(root):
        if manifest_path is None:
            print(f"SKIP (zip-form, not supported): {entry.name}")
            skipped_zip += 1
            continue
        if not manifest_path.exists():
            print(f"SKIP (no manifest.yaml): {entry.name}")
            continue

        meta = load_manifest(manifest_path)
        before = (meta.get("title", ""), meta.get("artist", ""), meta.get("album", ""))
        meta_repair.repair_meta(meta, entry.name)
        after = (meta.get("title", ""), meta.get("artist", ""), meta.get("album", ""))

        if meta.get("hidden"):
            print(f"JUNK (unfixable, hidden in library): {entry.name}")
            skipped_junk += 1
            continue
        if before == after:
            unchanged += 1
            continue

        changed += 1
        print(f"FIX: {entry.name}")
        print(f"     title  {before[0]!r} -> {after[0]!r}")
        if before[1] != after[1]:
            print(f"     artist {before[1]!r} -> {after[1]!r}")
        if before[2] != after[2]:
            print(f"     album  {before[2]!r} -> {after[2]!r}")

        if args.dry_run:
            continue

        meta.pop("hidden", None)  # scan-time flag, not manifest data
        bak = manifest_path.with_suffix(manifest_path.suffix + ".bak")
        if not bak.exists():
            shutil.copy2(manifest_path, bak)
        with open(manifest_path, "w", encoding="utf-8") as fh:
            yaml.safe_dump(meta, fh, allow_unicode=True, sort_keys=False, default_flow_style=False)

    print(f"\nDone ({'DRY RUN — no files written' if args.dry_run else 'written'}): "
          f"{changed} fixed, {skipped_junk} junk (unfixable), {skipped_zip} zip-form skipped, {unchanged} unchanged.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
