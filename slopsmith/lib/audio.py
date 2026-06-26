"""Audio extraction and conversion for Rocksmith CDLC."""

import os
import shutil
import subprocess
from pathlib import Path


def _vgmstream_cmd() -> str | None:
    """Return the path to vgmstream-cli if available."""
    return shutil.which("vgmstream-cli")


def _ffmpeg_cmd() -> str | None:
    """Return the path to ffmpeg if available."""
    return shutil.which("ffmpeg")


def find_wem_files(extracted_dir: str) -> list[str]:
    """Find WEM audio files, sorted largest first (full song before preview)."""
    wem_files = list(Path(extracted_dir).rglob("*.wem"))
    wem_files.sort(key=lambda p: p.stat().st_size, reverse=True)
    return [str(f) for f in wem_files]


def convert_wem(wem_path: str, output_base: str) -> str:
    """
    Convert a WEM file to a playable format.
    Returns path to the converted audio file.
    """
    # Try vgmstream-cli → WAV → MP3 (best browser compatibility)
    if shutil.which("vgmstream-cli"):
        wav = output_base + ".wav"
        r = subprocess.run(
            ["vgmstream-cli", "-o", wav, wem_path], capture_output=True
        )
        if r.returncode == 0 and os.path.exists(wav) and os.path.getsize(wav) > 0:
            if shutil.which("ffmpeg"):
                mp3 = output_base + ".mp3"
                r2 = subprocess.run(
                    ["ffmpeg", "-y", "-i", wav, "-b:a", "192k", mp3],
                    capture_output=True,
                )
                if r2.returncode == 0 and os.path.exists(mp3):
                    os.remove(wav)
                    return mp3
            return wav

    # Try ffmpeg directly (some builds handle Wwise)
    if shutil.which("ffmpeg"):
        mp3 = output_base + ".mp3"
        r = subprocess.run(
            ["ffmpeg", "-y", "-i", wem_path, "-b:a", "192k", mp3],
            capture_output=True,
        )
        if r.returncode == 0 and os.path.exists(mp3) and os.path.getsize(mp3) > 0:
            return mp3

        # Try WAV output as fallback
        wav = output_base + ".wav"
        r = subprocess.run(
            ["ffmpeg", "-y", "-i", wem_path, wav],
            capture_output=True,
        )
        if r.returncode == 0 and os.path.exists(wav) and os.path.getsize(wav) > 0:
            return wav

    # Try ww2ogg
    if shutil.which("ww2ogg"):
        ogg = output_base + ".ogg"
        r = subprocess.run(
            ["ww2ogg", wem_path, "-o", ogg], capture_output=True
        )
        if r.returncode == 0 and os.path.exists(ogg) and os.path.getsize(ogg) > 0:
            return ogg

    raise RuntimeError(
        "No WEM audio decoder found. Install vgmstream-cli:\n"
        "  Manjaro/Arch:  yay -S vgmstream-cli-bin\n"
        "  Or build from: github.com/vgmstream/vgmstream"
    )
