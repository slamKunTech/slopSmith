"""PSARC → sloppak conversion + stem splitting.

This module is the single source of truth for the convert + split pipelines.
Both the CLI scripts (`scripts/psarc_to_sloppak.py`, `scripts/split_stems.py`)
and the in-app converter plugin (`plugins/sloppak_converter`) import from
here — see the plugin's `routes.py` for the job queue that wraps these
functions with progress reporting.

Each function accepts a `progress_cb(fraction: float, stage: str, message: str)`
callback that the job queue forwards to the client over a WebSocket.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path
from typing import Callable, Optional

import yaml

from patcher import unpack_psarc
from song import load_song, arrangement_to_wire
from audio import find_wem_files, _vgmstream_cmd, _ffmpeg_cmd


ProgressCB = Optional[Callable[[float, str, str], None]]


# ── Shared helpers ────────────────────────────────────────────────────────────

def sanitize_stem(name: str) -> str:
    """Filesystem-safe version of a filename stem."""
    s = re.sub(r"[^A-Za-z0-9._-]+", "_", name).strip("_")
    return s or "song"


def _progress(cb: ProgressCB, frac: float, stage: str, msg: str) -> None:
    if cb:
        try:
            cb(frac, stage, msg)
        except Exception:
            pass


def _arrangement_id(name: str, used: set[str]) -> str:
    base = re.sub(r"[^a-z0-9]+", "_", name.lower()).strip("_") or "arr"
    candidate = base
    i = 2
    while candidate in used:
        candidate = f"{base}{i}"
        i += 1
    used.add(candidate)
    return candidate


def _wem_to_ogg(wem_path: str, out_ogg: Path) -> None:
    vgmstream = _vgmstream_cmd()
    ffmpeg = _ffmpeg_cmd()
    if not vgmstream:
        raise RuntimeError("vgmstream-cli not found on PATH")
    if not ffmpeg:
        raise RuntimeError("ffmpeg not found on PATH")

    with tempfile.TemporaryDirectory(prefix="s2p_wem_") as td:
        wav = Path(td) / "full.wav"
        r = subprocess.run([vgmstream, "-o", str(wav), wem_path], capture_output=True)
        if r.returncode != 0 or not wav.exists() or wav.stat().st_size < 100:
            raise RuntimeError(
                f"vgmstream-cli failed: {r.stderr.decode(errors='replace')}"
            )
        out_ogg.parent.mkdir(parents=True, exist_ok=True)
        r2 = subprocess.run(
            [ffmpeg, "-y", "-i", str(wav), "-c:a", "libvorbis", "-q:a", "5", str(out_ogg)],
            capture_output=True,
        )
        if r2.returncode != 0 or not out_ogg.exists() or out_ogg.stat().st_size < 100:
            raise RuntimeError(
                f"ffmpeg OGG encode failed: {r2.stderr.decode(errors='replace')}"
            )


def _parse_lyrics(extracted_dir: Path) -> list[dict]:
    # Try vocals XML first (CDLC and some official DLC)
    for xml_path in sorted(extracted_dir.rglob("*.xml")):
        try:
            root = ET.parse(xml_path).getroot()
        except Exception:
            continue
        if root.tag != "vocals":
            continue
        return [
            {
                "t": round(float(v.get("time", "0")), 3),
                "d": round(float(v.get("length", "0")), 3),
                "w": v.get("lyric", ""),
            }
            for v in root.findall("vocal")
        ]
    # Fall back to vocals SNG (official DLC ships SNG-only)
    try:
        from sng_vocals import parse_vocals_sng
        for sng_path in sorted(extracted_dir.rglob("*vocals*.sng")):
            plat = "mac" if "/macos/" in str(sng_path).replace("\\", "/").lower() else "pc"
            lyrics = parse_vocals_sng(str(sng_path), plat)
            if lyrics:
                return lyrics
    except ImportError:
        pass
    return []


def _extract_cover(extracted_dir: Path, out_jpg: Path) -> bool:
    dds_files = sorted(
        extracted_dir.rglob("*.dds"), key=lambda p: p.stat().st_size, reverse=True
    )
    if not dds_files:
        return False
    try:
        from PIL import Image
    except ImportError:
        return False
    try:
        img = Image.open(dds_files[0]).convert("RGB")
        out_jpg.parent.mkdir(parents=True, exist_ok=True)
        img.save(str(out_jpg), "JPEG", quality=88)
        return True
    except Exception:
        return False


def _zip_dir(src_dir: Path, out_zip: Path) -> None:
    out_zip.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(str(out_zip), "w", zipfile.ZIP_DEFLATED) as zf:
        for f in src_dir.rglob("*"):
            if f.is_file():
                zf.write(f, f.relative_to(src_dir).as_posix())


# ── PSARC → sloppak ───────────────────────────────────────────────────────────

def convert_psarc_to_sloppak(
    psarc_path: Path,
    out_path: Path,
    as_dir: bool = False,
    progress_cb: ProgressCB = None,
) -> Path:
    """Convert a PSARC to a .sloppak (single-stem). Returns the output path."""
    _progress(progress_cb, 0.02, "extracting", f"Unpacking {psarc_path.name}")
    tmp_extract = Path(tempfile.mkdtemp(prefix="s2p_extract_"))
    work_dir = Path(tempfile.mkdtemp(prefix="s2p_work_"))
    try:
        unpack_psarc(str(psarc_path), str(tmp_extract))

        _progress(progress_cb, 0.15, "extracting", "Parsing song data")
        song = load_song(str(tmp_extract))
        if not song.arrangements:
            raise RuntimeError("no playable arrangements found in PSARC")

        used_ids: set[str] = set()
        arr_manifest: list[dict] = []
        first = True
        for arr in song.arrangements:
            aid = _arrangement_id(arr.name, used_ids)
            wire = arrangement_to_wire(arr)
            if first:
                wire["beats"] = [
                    {"time": round(b.time, 3), "measure": b.measure} for b in song.beats
                ]
                wire["sections"] = [
                    {"name": s.name, "number": s.number, "time": round(s.start_time, 3)}
                    for s in song.sections
                ]
                first = False
            arr_file = work_dir / "arrangements" / f"{aid}.json"
            arr_file.parent.mkdir(parents=True, exist_ok=True)
            arr_file.write_text(json.dumps(wire, separators=(",", ":")), encoding="utf-8")
            arr_manifest.append({
                "id": aid,
                "name": arr.name,
                "file": f"arrangements/{aid}.json",
                "tuning": list(arr.tuning),
                "capo": arr.capo,
            })

        _progress(progress_cb, 0.35, "extracting", "Converting audio (WEM → OGG)")
        wems = find_wem_files(str(tmp_extract))
        if not wems:
            raise RuntimeError("no WEM audio found in PSARC")
        _wem_to_ogg(wems[0], work_dir / "stems" / "full.ogg")

        stems_manifest = [{"id": "full", "file": "stems/full.ogg", "default": "on"}]

        lyrics = _parse_lyrics(tmp_extract)
        lyrics_rel = None
        if lyrics:
            (work_dir / "lyrics.json").write_text(
                json.dumps(lyrics, separators=(",", ":")), encoding="utf-8"
            )
            lyrics_rel = "lyrics.json"

        cover_rel = None
        if _extract_cover(tmp_extract, work_dir / "cover.jpg"):
            cover_rel = "cover.jpg"

        manifest: dict = {
            "title": song.title or psarc_path.stem,
            "artist": song.artist or "",
            "album": song.album or "",
            "year": int(song.year or 0),
            "duration": round(float(song.song_length or 0.0), 3),
        }
        if cover_rel:
            manifest["cover"] = cover_rel
        manifest["stems"] = stems_manifest
        manifest["arrangements"] = arr_manifest
        if lyrics_rel:
            manifest["lyrics"] = lyrics_rel
        (work_dir / "manifest.yaml").write_text(
            yaml.safe_dump(manifest, sort_keys=False, allow_unicode=True),
            encoding="utf-8",
        )

        _progress(progress_cb, 0.85, "packing", "Writing output")
        if as_dir:
            if out_path.exists():
                shutil.rmtree(out_path)
            shutil.copytree(work_dir, out_path)
        else:
            _zip_dir(work_dir, out_path)

        _progress(progress_cb, 1.0, "done", f"Wrote {out_path.name}")
        return out_path
    finally:
        shutil.rmtree(tmp_extract, ignore_errors=True)
        shutil.rmtree(work_dir, ignore_errors=True)


# ── Stem splitting via Demucs ────────────────────────────────────────────────

_STEM_ORDER = ["guitar", "bass", "drums", "vocals", "piano", "other"]


def demucs_available() -> bool:
    try:
        import demucs  # noqa: F401
        return True
    except ImportError:
        return False


def _get_demucs_server_url() -> str | None:
    """Get the configured remote demucs server URL, if any."""
    config_dir = Path(os.environ.get("CONFIG_DIR", "/config"))
    config_file = config_dir / "config.json"
    if config_file.exists():
        try:
            import json
            cfg = json.loads(config_file.read_text())
            url = cfg.get("demucs_server_url", "")
            if url:
                return url.rstrip("/")
        except Exception:
            pass
    return None


def _run_demucs_remote(full_ogg: Path, out_dir: Path, model: str) -> Path:
    """Run stem separation via remote demucs server."""
    import json
    import requests

    server_url = _get_demucs_server_url()
    if not server_url:
        raise RuntimeError("No demucs server configured")

    # Upload the audio file — request all stems the model can produce
    stem_list = "drums,bass,vocals,other,guitar,piano"
    with open(full_ogg, "rb") as f:
        resp = requests.post(
            f"{server_url}/separate",
            files={"file": (full_ogg.name, f, "audio/ogg")},
            params={"model": model, "stems": stem_list},
            timeout=600,
        )

    if resp.status_code != 200:
        raise RuntimeError(f"Demucs server error: {resp.text[:300]}")

    data = resp.json()
    stems = data.get("stems", {})
    if not stems:
        # Might be a job-based response — poll for completion
        job_id = data.get("job_id")
        if job_id:
            import time
            for _ in range(120):  # Wait up to 10 minutes
                time.sleep(5)
                jr = requests.get(f"{server_url}/jobs/{job_id}", timeout=30)
                jd = jr.json()
                if jd.get("status") == "complete":
                    stems = jd.get("stems", {})
                    break
                elif jd.get("status") == "failed":
                    raise RuntimeError(f"Demucs server job failed: {jd.get('error')}")

    if not stems:
        raise RuntimeError("Demucs server returned no stems")

    # Download each stem
    result_dir = out_dir / "remote_stems"
    result_dir.mkdir(parents=True, exist_ok=True)
    for stem_name, stem_url in stems.items():
        if stem_url.startswith("/"):
            stem_url = f"{server_url}{stem_url}"
        sr = requests.get(stem_url, timeout=120)
        if sr.status_code == 200:
            ext = ".mp3" if ".mp3" in stem_url else ".wav"
            (result_dir / f"{stem_name}{ext}").write_bytes(sr.content)

    return result_dir


def _run_demucs(full_ogg: Path, out_dir: Path, model: str) -> Path:
    out_dir.mkdir(parents=True, exist_ok=True)
    cmd = [sys.executable, "-m", "demucs", "-n", model, "-o", str(out_dir), str(full_ogg)]
    # Point model-weight caches at the persistent config volume so we don't
    # re-download on every container restart (~300MB per model).
    env = os.environ.copy()
    config_dir = env.get("CONFIG_DIR", "/config")
    cache_root = Path(config_dir) / "torch_cache"
    cache_root.mkdir(parents=True, exist_ok=True)
    env.setdefault("TORCH_HOME", str(cache_root))
    env.setdefault("XDG_CACHE_HOME", str(cache_root))
    # Propagate in-process sys.path additions (plugin loader adds
    # /config/pip_packages at runtime, not via PYTHONPATH) so the child
    # python can also find demucs/torch/torchcodec.
    pip_target = str(Path(config_dir) / "pip_packages")
    extra_paths = [p for p in sys.path if p and p != ""]
    merged = os.pathsep.join(
        [pip_target] + [p for p in extra_paths if p != pip_target]
        + ([env["PYTHONPATH"]] if env.get("PYTHONPATH") else [])
    )
    env["PYTHONPATH"] = merged
    r = subprocess.run(cmd, env=env, capture_output=True, text=True)
    if r.returncode != 0:
        err_tail = (r.stderr or "").strip().splitlines()[-5:]
        raise RuntimeError(
            f"demucs exited with code {r.returncode}: " + " | ".join(err_tail)
        )
    track_stem = full_ogg.stem
    result_dir = out_dir / model / track_stem
    if not result_dir.exists():
        candidates = list((out_dir / model).iterdir()) if (out_dir / model).exists() else []
        if len(candidates) == 1 and candidates[0].is_dir():
            result_dir = candidates[0]
        else:
            raise RuntimeError(f"demucs output dir not found under {out_dir}/{model}")
    return result_dir


def _encode_ogg(wav_path: Path, ogg_path: Path) -> None:
    ffmpeg = _ffmpeg_cmd() or "ffmpeg"
    ogg_path.parent.mkdir(parents=True, exist_ok=True)
    r = subprocess.run(
        [ffmpeg, "-y", "-i", str(wav_path),
         "-c:a", "libvorbis", "-q:a", "5", str(ogg_path)],
        capture_output=True,
    )
    if r.returncode != 0 or not ogg_path.exists():
        raise RuntimeError(
            f"ffmpeg OGG encode failed for {wav_path.name}: "
            f"{r.stderr.decode(errors='replace')}"
        )


def _rewrite_stems_manifest(source_dir: Path, new_stems: list[dict]) -> None:
    mf = source_dir / "manifest.yaml"
    if not mf.exists():
        mf = source_dir / "manifest.yml"
    data = yaml.safe_load(mf.read_text(encoding="utf-8")) or {}
    data["stems"] = new_stems
    mf.write_text(
        yaml.safe_dump(data, sort_keys=False, allow_unicode=True),
        encoding="utf-8",
    )


def _split_in_dir(
    source_dir: Path,
    model: str,
    progress_cb: ProgressCB,
    base_frac: float,
    span_frac: float,
) -> None:
    full_ogg = source_dir / "stems" / "full.ogg"
    if not full_ogg.exists():
        raise FileNotFoundError(
            f"{full_ogg} not found — run PSARC conversion first or add stems/full.ogg."
        )

    # Try remote demucs server first, fall back to local
    remote_url = _get_demucs_server_url()
    use_remote = remote_url is not None

    if use_remote:
        _progress(progress_cb, base_frac + span_frac * 0.05, "splitting",
                  f"Sending to Demucs server ({remote_url})")
    else:
        _progress(progress_cb, base_frac + span_frac * 0.05, "splitting",
                  f"Running Demucs locally ({model})")

    with tempfile.TemporaryDirectory(prefix="s2p_split_") as td:
        if use_remote:
            try:
                result_dir = _run_demucs_remote(full_ogg, Path(td), model)
            except Exception as e:
                print(f"[Demucs] Remote failed ({e}), falling back to local")
                if demucs_available():
                    result_dir = _run_demucs(full_ogg, Path(td), model)
                else:
                    raise RuntimeError(f"Remote demucs failed and local demucs not available: {e}")
        else:
            result_dir = _run_demucs(full_ogg, Path(td), model)

        _progress(progress_cb, base_frac + span_frac * 0.85, "splitting",
                  "Encoding split stems")
        produced: list[dict] = []
        stems_dir = source_dir / "stems"
        for wav in sorted(result_dir.glob("*.wav")):
            name = wav.stem.lower()
            out_ogg = stems_dir / f"{name}.ogg"
            _encode_ogg(wav, out_ogg)
            produced.append({"id": name, "file": f"stems/{name}.ogg", "default": "on"})

    if not produced:
        raise RuntimeError("demucs produced no output stems")

    def _order_key(s: dict) -> tuple[int, str]:
        try:
            return (_STEM_ORDER.index(s["id"]), s["id"])
        except ValueError:
            return (len(_STEM_ORDER), s["id"])
    produced.sort(key=_order_key)

    full_ogg.unlink(missing_ok=True)
    _rewrite_stems_manifest(source_dir, produced)


def split_sloppak_stems(
    sloppak_path: Path,
    model: str = "htdemucs_6s",
    progress_cb: ProgressCB = None,
    base_frac: float = 0.0,
    span_frac: float = 1.0,
) -> None:
    """Split a sloppak's stems/full.ogg into per-instrument stems via Demucs."""
    if sloppak_path.is_dir():
        _split_in_dir(sloppak_path, model, progress_cb, base_frac, span_frac)
        return

    # Zip form: unpack, split, re-zip atomically.
    with tempfile.TemporaryDirectory(prefix="s2p_split_zip_") as td:
        work = Path(td) / "sloppak"
        work.mkdir()
        with zipfile.ZipFile(str(sloppak_path), "r") as zf:
            zf.extractall(work)

        _split_in_dir(work, model, progress_cb, base_frac, span_frac * 0.9)

        _progress(progress_cb, base_frac + span_frac * 0.95, "packing",
                  "Repacking sloppak")
        tmp_out = sloppak_path.with_suffix(sloppak_path.suffix + ".tmp")
        with zipfile.ZipFile(str(tmp_out), "w", zipfile.ZIP_DEFLATED) as zf:
            for f in work.rglob("*"):
                if f.is_file():
                    zf.write(f, f.relative_to(work).as_posix())
        tmp_out.replace(sloppak_path)
