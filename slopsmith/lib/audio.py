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


def encode_wav_to_ogg(
    wav_path: str | Path,
    ogg_path: str | Path,
    quality: int = 5,
    ffmpeg: str | None = None,
) -> None:
    """Encode a WAV (or any ffmpeg-decodable audio) to Ogg/Vorbis.

    Prefers the ``libvorbis`` encoder; falls back to ffmpeg's built-in
    (experimental) ``vorbis`` encoder when the ffmpeg build doesn't link
    libvorbis (e.g. stock Homebrew ffmpeg). Raises RuntimeError on failure.
    """
    ff = ffmpeg or _ffmpeg_cmd() or "ffmpeg"
    ogg_path = Path(ogg_path)
    ogg_path.parent.mkdir(parents=True, exist_ok=True)

    # Primary: libvorbis
    r = subprocess.run(
        [ff, "-y", "-loglevel", "error", "-i", str(wav_path),
         "-c:a", "libvorbis", "-q:a", str(quality), str(ogg_path)],
        capture_output=True,
    )
    if r.returncode == 0 and ogg_path.exists() and ogg_path.stat().st_size > 0:
        return

    # Fallback: native vorbis (experimental -> needs -strict -2)
    if ogg_path.exists() and ogg_path.stat().st_size == 0:
        ogg_path.unlink()
    r2 = subprocess.run(
        [ff, "-y", "-loglevel", "error", "-i", str(wav_path),
         "-c:a", "vorbis", "-strict", "-2", "-q:a", str(quality), str(ogg_path)],
        capture_output=True,
    )
    if r2.returncode != 0 or not ogg_path.exists() or ogg_path.stat().st_size < 100:
        raise RuntimeError(
            f"ffmpeg OGG/Vorbis encode failed for {Path(wav_path).name}: "
            f"{r2.stderr.decode(errors='replace')[-400:]}"
        )


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
