#!/usr/bin/env python3
"""Stage A: batch MIDI -> GP5 converter.

Mirrors the source directory tree under the destination, converts every
.mid/.midi file through convert_one.py in parallel subprocesses with a
per-file timeout, and writes a JSON report (`_convert_report.json` under the
destination unless --report overrides).

Usage:
    python batch_convert.py SRC_DIR DST_DIR
        [--python PATH]     # interpreter that can run convert_one.py
        [--jobs N]          # parallel subprocesses (default 6)
        [--timeout SEC]     # per-file kill timeout (default 180)
        [--overwrite]       # redo files that already have non-empty output
        [--report FILE]
"""
import argparse
import json
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

HERE = Path(__file__).resolve().parent
WORKER = HERE / "convert_one.py"
# Stage-A interpreter: needs pretty_midi + PyGuitarPro installed.
DEFAULT_PY = "/Users/mac/glbpy14/glbpy14/bin/python3"


def find_midis(src: Path) -> list[Path]:
    return sorted(
        p for p in src.rglob("*")
        if p.suffix.lower() in (".mid", ".midi") and p.is_file()
    )


def convert_one(mid: Path, src: Path, dst: Path, py: str,
                timeout: int, overwrite: bool) -> dict:
    rel = mid.relative_to(src)
    out = dst / rel.with_suffix(".gp5")
    if out.exists() and out.stat().st_size > 0 and not overwrite:
        return {"file": str(rel), "status": "skipped"}
    try:
        r = subprocess.run(
            [py, str(WORKER), str(mid), str(out)],
            capture_output=True, text=True, timeout=timeout,
        )
        if r.returncode == 0:
            return {"file": str(rel), "status": "ok", "log": r.stdout.strip()[-200:]}
        return {"file": str(rel), "status": "fail",
                "error": (r.stderr.strip() or r.stdout.strip())[-500:]}
    except subprocess.TimeoutExpired:
        return {"file": str(rel), "status": "timeout", "error": f"exceeded {timeout}s"}
    except Exception as e:
        return {"file": str(rel), "status": "error", "error": str(e)}


def run_batch(src: Path, dst: Path, py: str = DEFAULT_PY, jobs: int = 6,
              timeout: int = 180, overwrite: bool = False,
              report: Path | None = None) -> dict:
    """Convert all MIDIs under src into dst. Returns the report dict."""
    midis = find_midis(src)
    print(f"Found {len(midis)} MIDI files under {src}", flush=True)

    results: list[dict] = []
    t0 = time.time()
    ok = fail = skip = 0
    with ThreadPoolExecutor(max_workers=jobs) as ex:
        futs = {ex.submit(convert_one, m, src, dst, py, timeout, overwrite): m
                for m in midis}
        for i, fut in enumerate(as_completed(futs), 1):
            res = fut.result()
            results.append(res)
            if res["status"] == "ok":
                ok += 1
            elif res["status"] == "skipped":
                skip += 1
            else:
                fail += 1
                print(f"[FAIL] {res['file']}: {res.get('error', '')[:200]}", flush=True)
            if i % 25 == 0 or i == len(midis):
                print(f"progress {i}/{len(midis)} ok={ok} fail={fail} skip={skip} "
                      f"elapsed={time.time() - t0:.0f}s", flush=True)

    results.sort(key=lambda r: r["file"])
    rep = {"total": len(midis), "ok": ok, "fail": fail, "skipped": skip,
           "results": results}
    report = report or dst / "_convert_report.json"
    report.parent.mkdir(parents=True, exist_ok=True)
    report.write_text(json.dumps(rep, ensure_ascii=False, indent=1))
    print(f"DONE ok={ok} fail={fail} skip={skip} in {time.time() - t0:.0f}s; "
          f"report -> {report}", flush=True)
    return rep


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("src", type=Path)
    ap.add_argument("dst", type=Path)
    ap.add_argument("--python", default=DEFAULT_PY)
    ap.add_argument("--jobs", type=int, default=6)
    ap.add_argument("--timeout", type=int, default=180)
    ap.add_argument("--overwrite", action="store_true")
    ap.add_argument("--report", type=Path, default=None)
    args = ap.parse_args()
    rep = run_batch(args.src, args.dst, args.python, args.jobs,
                    args.timeout, args.overwrite, args.report)
    sys.exit(1 if rep["fail"] else 0)


if __name__ == "__main__":
    main()
