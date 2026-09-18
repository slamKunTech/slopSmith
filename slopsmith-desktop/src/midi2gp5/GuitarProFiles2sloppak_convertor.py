#!/usr/bin/env python3
"""Incremental batch converter: GuitarPro8GPTfiles GP files -> sloppak_converted.

Traverses the source dir for .gp3/.gp4/.gp5 files. For each file it computes
the sloppak name the main converter would produce (filename stem + 6-char
path hash, same rule as slopsmith's convert_gp_to_sloppak._slug_for) and
checks whether OUT/<slug>.sloppak/manifest.yaml already exists at the
corresponding position. Already-converted songs are skipped; the rest
(incremental) are converted.

Reuses slopsmith's convert_one from scripts/convert_gp_to_sloppak.py, so
run with the slopsmith venv python:
    /Users/mac/codes/slopSmith/slopsmith/.venv/bin/python \
        GuitarProFiles2sloppak_convertor.py

Usage:
    python GuitarProFiles2sloppak_convertor.py [--src DIR] [--out DIR]
        [--workers N] [--limit N] [--force] [--list-missing]
"""
import argparse
import functools
import sys
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

# ── Defaults ──────────────────────────────────────────────────────────────
SLOPPY_ROOT = Path("/Users/mac/codes/slopSmith/slopsmith")
DEFAULT_SRC = Path("/Users/mac/tridu33/guitarProScore/GuitarPro8GPTfiles")
DEFAULT_OUT = Path("/Users/mac/tridu33/guitarProScore/sloppak_converted")

sys.path.insert(0, str(SLOPPY_ROOT / "scripts"))  # module also inserts lib/
from convert_gp_to_sloppak import convert_one, _slug_for  # noqa: E402

GP_SUFFIXES = (".gp3", ".gp4", ".gp5")


def find_gp_files(src: Path) -> list[Path]:
    """All GP files under src, sorted for a stable conversion order."""
    return sorted(p for p in src.rglob("*")
                  if p.is_file() and p.suffix.lower() in GP_SUFFIXES)


def is_converted(gp: Path, out: Path) -> bool:
    """True when the sloppak this GP file maps to already has a manifest."""
    spath = out / f"{_slug_for(gp)}.sloppak"
    return (spath / "manifest.yaml").is_file()


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--src", type=Path, default=DEFAULT_SRC, help="GP source dir")
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT, help="sloppak output dir")
    ap.add_argument("--workers", type=int, default=2, help="parallel processes")
    ap.add_argument("--limit", type=int, default=0, help="convert at most N files")
    ap.add_argument("--force", action="store_true", help="reconvert everything, ignore existing")
    ap.add_argument("--list-missing", action="store_true",
                    help="only print the GP files missing sloppaks, don't convert")
    args = ap.parse_args()

    src, out = args.src, args.out
    out.mkdir(parents=True, exist_ok=True)
    files = find_gp_files(src)

    if args.force:
        todo = files
    else:
        todo = [f for f in files if not is_converted(f, out)]
    print(f"src: {src}")
    print(f"out: {out}")
    print(f"GP files: {len(files)}  already converted: {len(files) - len(todo)}  "
          f"to convert: {len(todo)}", flush=True)

    if args.list_missing or not todo:
        for f in todo:
            print(f"  MISSING {f.relative_to(src)}", flush=True)
        return

    if args.limit > 0:
        todo = todo[:args.limit]

    if args.workers > 1:
        worker = functools.partial(convert_one, out_dir=out)
        with ProcessPoolExecutor(max_workers=args.workers) as executor:
            results = list(executor.map(worker, todo))
    else:
        results = [convert_one(f, out) for f in todo]

    ok = sum(1 for r in results if r)
    failed = [f.name for f, r in zip(todo, results) if not r]
    print(f"Done: {ok}/{len(todo)} converted.")
    if failed:
        print("Failed:")
        for name in failed:
            print(f"  ✗ {name}")


if __name__ == "__main__":
    main()
