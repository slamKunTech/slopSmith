#!/usr/bin/env python3
"""Repair junk titles in unpacked .sloppak dirs from their source GP filenames.

The early GP converter wrote the GP file's internal title verbatim into the
manifest; many GP files carry junk metadata ('-', 'Untitled', 'K-', ...), so
the library shows hundreds of unusable titles even though the .sloppak
directory name (source filename + short hash) holds the real name.

Per directory:
  * manifest exists and its title is junk/mojibake → rewrite title/artist
    parsed from the directory name; artist also fixed when junk.
  * manifest missing and the stem matches a source GP file → reconvert
    that file (convert_gp_to_sloppak.convert_one, which now falls back to
    the filename stem for junk titles).
  * otherwise the correct name cannot be recovered → delete the directory
    (recorded in <sloppak_dir>/repair_log.txt).

Usage (run from the slopsmith repo root):
    python scripts/repair_gp_titles.py <sloppak_dir> <gp_source_dir> [--dry-run]
"""
import argparse
import os
import re
import shutil
import sys
import time
from pathlib import Path

# Make lib/ importable regardless of CWD.
_HERE = Path(__file__).resolve().parent
_ROOT = _HERE.parent
sys.path.insert(0, str(_ROOT / "lib"))
import yaml  # noqa: E402
from meta_repair import (  # noqa: E402
    clean_filename_stem,
    is_junk_artist,
    is_junk_title,
    parse_source_stem,
    repair_text,
)

def _needs_title_fix(t: str) -> bool:
    """True when the manifest title cannot be trusted (junk / mojibake /
    converter scrap)."""
    s = (t or "").strip()
    if not s:
        return True
    if is_junk_title(s):
        return True
    if repair_text(s) != s:
        return True
    return False


def _needs_artist_fix(a: str) -> bool:
    return is_junk_artist(a)


# Filesystem-level mojibake that can't be recovered by any encoding roundtrip.
# Each entry: (needle in the .sloppak dir name, title override, artist
# override). None means "keep the value parsed from the dir name".
_MOJIBAKE_OVERRIDES = [
    ("《��失幻境》", "《迷失幻境》", None),
    ("《��情种》", "《多情种》", None),
    ("《离开地球表面》-���月天", "《离开地球表面》", "五月天"),
    ("《龙卷风》-��杰伦", "《龙卷风》", "周杰伦"),
    ("《冬天的秘密���", "《冬天的秘密》", None),
]


def derived_name_for(dir_name: str) -> tuple[str, str]:
    """(title, artist) for a .sloppak dir name: parse the cleaned stem,
    then apply any filesystem-mojibake overrides."""
    derived = parse_source_stem(clean_filename_stem(dir_name))
    for needle, otitle, oartist in _MOJIBAKE_OVERRIDES:
        if needle in dir_name:
            derived = (otitle or derived[0], oartist or derived[1])
            break
    return derived


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("sloppak_dir", help="directory of unpacked *.sloppak dirs")
    ap.add_argument("gp_source_dir", help="directory of original GP files/dirs")
    ap.add_argument("--dry-run", action="store_true", help="only print the plan")
    args = ap.parse_args()

    sloppak_dir = Path(args.sloppak_dir).resolve()
    src_dir = Path(args.gp_source_dir).resolve()
    if not sloppak_dir.is_dir():
        print(f"✗ sloppak dir not found: {sloppak_dir}", flush=True)
        return 1
    if not src_dir.is_dir():
        print(f"✗ source dir not found: {src_dir}", flush=True)
        return 1

    # Source lookup: GP-file stems -> paths, plus directory names -> dirs.
    file_by_stem: dict[str, list[Path]] = {}
    dirs_by_name: dict[str, list[Path]] = {}
    for p in sorted(src_dir.rglob("*")):
        if p.is_file() and p.suffix.lower() in (".gp3", ".gp4", ".gp5", ".gpx", ".gp"):
            file_by_stem.setdefault(p.stem, []).append(p)
        elif p.is_dir() and p != src_dir:
            dirs_by_name.setdefault(p.name, []).append(p)

    # Gather actions. Walk recursively so sloppaks nested in subfolders (e.g.
    # a per-tuning subdirectory) are repaired too, not silently skipped.
    actions = []  # (kind, rel_path, detail)
    for entry in sorted(p for p in sloppak_dir.rglob("*")
                        if p.is_dir() and p.name.lower().endswith(".sloppak")):
        rel = entry.relative_to(sloppak_dir).as_posix()
        stem = clean_filename_stem(entry.name)
        mf = entry / "manifest.yaml"

        meta = None
        if mf.is_file():
            try:
                loaded = yaml.safe_load(mf.read_text(encoding="utf-8")) or {}
                if isinstance(loaded, dict):
                    meta = loaded
            except Exception as e:
                print(f"  ! {entry.name}: manifest unreadable ({e})", flush=True)

        title_ok = bool(meta) and not _needs_title_fix(str(meta.get("title", "")))
        derived = derived_name_for(entry.name)
        derived_usable = (
            bool(derived[0])
            and not is_junk_title(derived[0])
            and repair_text(derived[0]) == derived[0]
        )
        if not derived_usable:
            # parse_source_stem couldn't split a usable title (e.g. a leading
            # id prefix like '7710-...'). Fall back to the whole cleaned stem
            # as the title — readable and far better than a '?' mojibake title
            # — instead of deleting a dir that still holds valid audio.
            if stem and not is_junk_title(stem) and repair_text(stem) == stem:
                derived = (stem, derived[1] or "")
                derived_usable = True

        if title_ok:
            # Title fine; fix artist only when junk and we have a better one.
            if derived_usable and derived[1] and _needs_artist_fix(str(meta.get("artist", ""))):
                actions.append(("artist", rel, f"{meta.get('artist')!r} → {derived[1]!r}"))
            continue

        if derived_usable:
            if meta is None:
                # No manifest: try to reconvert from the source GP file.
                src = None
                if file_by_stem.get(stem):
                    src = file_by_stem[stem][0]
                elif dirs_by_name.get(stem):
                    d = dirs_by_name[stem][0]
                    gps = sorted(
                        p for p in d.rglob("*")
                        if p.is_file() and p.suffix.lower() in (".gp3", ".gp4", ".gp5", ".gpx", ".gp")
                    )
                    if gps:
                        src = gps[0]
                if src is not None:
                    actions.append(("reconvert", rel, f"from {src}", src))
                else:
                    actions.append(("delete", rel, "no manifest, no matching source"))
            else:
                actions.append(("rewrite", rel,
                                f"title {meta.get('title')!r} → {derived[0]!r}, "
                                f"artist {meta.get('artist')!r} → {derived[1]!r}"))
        else:
            # Title junk AND dir name yields nothing usable → delete.
            actions.append(("delete", rel, f"unrecoverable name (stem {stem!r})"))

    n_rewrite = sum(1 for a in actions if a[0] == "rewrite")
    n_artist = sum(1 for a in actions if a[0] == "artist")
    n_reconvert = sum(1 for a in actions if a[0] == "reconvert")
    n_delete = sum(1 for a in actions if a[0] == "delete")
    print(f"plan: {n_rewrite} rewrite, {n_artist} artist-only, "
          f"{n_reconvert} reconvert, {n_delete} delete", flush=True)

    if args.dry_run:
        for kind, name, detail, *_ in actions:
            print(f"  [{kind:>9}] {name}  {detail}", flush=True)
        return 0

    log_path = sloppak_dir / "repair_log.txt"
    log_lines = []
    done = {"rewrite": 0, "artist": 0, "reconvert": 0, "delete": 0, "error": 0}
    for kind, name, detail, *rest in actions:
        entry = sloppak_dir / name
        try:
            if kind in ("rewrite", "artist") and entry.is_dir():
                mf = entry / "manifest.yaml"
                meta = yaml.safe_load(mf.read_text(encoding="utf-8")) or {}
                derived = derived_name_for(Path(name).name)
                # Same whole-stem fallback as the planning pass: when the
                # parsed title is junk (leading id prefix like '7710-...'),
                # use the whole cleaned stem so the manifest gets a readable
                # name instead of a junk fragment.
                if is_junk_title(derived[0]) or repair_text(derived[0]) != derived[0]:
                    raw = clean_filename_stem(Path(name).name)
                    if raw and not is_junk_title(raw) and repair_text(raw) == raw:
                        derived = (raw, derived[1] or "")
                if kind == "rewrite":
                    meta["title"] = derived[0]
                if derived[1]:
                    meta["artist"] = derived[1]
                mf.write_text(yaml.safe_dump(meta, sort_keys=False, allow_unicode=True),
                              encoding="utf-8")
                # The library scan keys on the sloppak dir's (mtime, size);
                # editing a file inside does not change either, so bump the
                # dir mtime to force a rescan of this entry.
                os.utime(entry, None)
            elif kind == "reconvert" and entry.is_dir():
                src = rest[0]
                # Import here: pulls guitarpro/gp2rs/gp2midi, slow to load.
                from convert_gp_to_sloppak import convert_one, _slug_for  # noqa: PLC0415
                out = convert_one(src, sloppak_dir)
                if out is None:
                    # Conversion failed (parse/gp2rs/empty): nothing worth
                    # keeping — remove the manifest-less original and any
                    # partial dir the converter may have started.
                    shutil.rmtree(entry, ignore_errors=True)
                    partial = sloppak_dir / f"{_slug_for(src)}.sloppak"
                    if partial.resolve() != entry.resolve():
                        shutil.rmtree(partial, ignore_errors=True)
                    done["error"] += 1
                    log_lines.append(f"[reconvert-failed] {name} from {src}")
                    continue
                # convert_one writes to slug-of(src); if it differs from the
                # original dir (path changed since first conversion), remove
                # the manifest-less original.
                if entry.is_dir() and entry.resolve() != Path(out).resolve():
                    shutil.rmtree(entry, ignore_errors=True)
            elif kind == "delete" and entry.is_dir():
                # Never permanently delete user data — move to the OS trash.
                trash = Path.home() / ".Trash"
                trash.mkdir(parents=True, exist_ok=True)
                dest = trash / entry.name
                if dest.exists():
                    dest = trash / f"{entry.name}.{int(time.time())}"
                shutil.move(str(entry), str(dest))
            done[kind] += 1
            log_lines.append(f"[{kind}] {name}  {detail}")
        except Exception as e:
            done["error"] += 1
            log_lines.append(f"[error] {name}  {e}")

    log_path.write_text("\n".join(log_lines) + "\n", encoding="utf-8")
    print(f"done: {done}", flush=True)
    print(f"log: {log_path}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
