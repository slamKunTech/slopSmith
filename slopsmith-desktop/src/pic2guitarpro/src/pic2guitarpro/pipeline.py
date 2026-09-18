"""Orchestrator: tab-image folder -> .gp5 files in output/.

Input layout (per the project spec)::

    <input_folder>/
      <Song Name 1>/        # subfolder name = song name
        ..._I.png           # pages ordered by Roman-numeral suffix
        ..._II.png
        ..._III.png
      <Song Name 2>/
        ...

Each subfolder is one song. Its images are OCR'd in Roman-numeral order,
concatenated, parsed into a :class:`ParsedTab`, and written as
``<output>/<Song Name>.gp5``. The intermediate ASCII is also saved as
``<output>/<Song Name>.txt`` for inspection.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from concurrent.futures import ThreadPoolExecutor

import anthropic
from ocr_tabber.ocr_tab import ocr_tab_image

from .ascii_parser import parse_tab
from .roman import sort_by_roman
from .song_builder import write_gp5
from .vision_transcriber import DEFAULT_MODEL, transcribe_image

SUPPORTED_IMAGE_EXTS = {".png", ".jpg", ".jpeg", ".gif", ".bmp", ".tiff", ".tif", ".webp"}

# Vision transcription is I/O-bound (one API call per image), so a thread pool
# gives a near-linear speedup. 8 is a safe default for the DashScope relay.
DEFAULT_CONCURRENCY = 8


def transcribe(image_path: Path, *, method: str = "vision", model: str = DEFAULT_MODEL,
               client: anthropic.Anthropic | None = None) -> str:
    """Transcribe a tab image to ASCII tab.

    ``method="vision"`` (default) uses Claude/Qwen vision — required for
    engraved staves. ``method="tesseract"`` uses OCR-tabber — only works on
    ASCII-tab style images.
    """
    if method == "tesseract":
        return ocr_tab_image(str(image_path))
    return transcribe_image(image_path, model=model, client=client)




@dataclass
class SongResult:
    name: str
    gp5_path: Path | None
    txt_path: Path | None
    n_images: int
    n_measures: int
    error: str | None = None


def _is_image(p: Path) -> bool:
    return p.suffix.lower() in SUPPORTED_IMAGE_EXTS and p.is_file()


def _song_dirs(input_folder: Path) -> list[tuple[str, Path]]:
    """Return ``(song_name, dir)`` pairs.

    A subfolder qualifies as a song if it contains at least one image. If the
    input folder itself contains images directly (no subfolders), treat the
    input folder as a single song named after it.
    """
    input_folder = input_folder.resolve()
    direct_images = [p for p in input_folder.iterdir() if _is_image(p)]
    subdirs = [p for p in input_folder.iterdir() if p.is_dir()]

    if direct_images and not subdirs:
        return [(input_folder.name, input_folder)]

    songs: list[tuple[str, Path]] = []
    for d in sorted(subdirs, key=lambda p: p.name):
        if any(_is_image(p) for p in d.iterdir()):
            songs.append((d.name, d))
    return songs


def process_song(
    name: str,
    song_dir: Path,
    output_dir: Path,
    *,
    method: str = "vision",
    model: str = DEFAULT_MODEL,
    concurrency: int = DEFAULT_CONCURRENCY,
) -> SongResult:
    """Transcribe, parse, and write one song's .gp5 (and .txt) into ``output_dir/<song>/``."""
    song_out = output_dir / name
    song_out.mkdir(parents=True, exist_ok=True)
    images = [p for p in song_dir.iterdir() if _is_image(p)]
    images_sorted = [Path(p) for p in sort_by_roman([str(p) for p in images])]

    # Transcribe images in parallel (I/O-bound API calls), preserving order.
    # A per-call timeout keeps a throttled relay from stalling a whole song:
    # an image that times out is skipped (logged), and the song is assembled
    # from the remaining images.
    client = (
        anthropic.Anthropic(timeout=300.0, max_retries=1)
        if method == "vision" else None
    )
    ascii_chunks: list[str | None] = [None] * len(images_sorted)
    ocr_errors: list[str] = []
    with ThreadPoolExecutor(max_workers=concurrency) as pool:
        future_to_idx = {
            pool.submit(transcribe, img, method=method, model=model, client=client): i
            for i, img in enumerate(images_sorted)
        }
        for fut, i in ((f, future_to_idx[f]) for f in future_to_idx):
            try:
                ascii_chunks[i] = fut.result()
            except Exception as e:  # noqa: BLE001
                ocr_errors.append(f"{images_sorted[i].name}: {e}")
    ascii_text = "\n".join(c for c in ascii_chunks if c)

    txt_path = song_out / f"{name}.txt"
    txt_path.write_text(ascii_text + ("\n# OCR errors:\n" + "\n".join(ocr_errors) if ocr_errors else ""))

    if not ascii_text.strip():
        return SongResult(name, None, txt_path, len(images_sorted), 0,
                          error="no OCR text extracted from images")

    tab = parse_tab(ascii_text)
    gp5_path = song_out / f"{name}.gp5"
    try:
        write_gp5(tab, gp5_path, title=name)
    except Exception as e:  # noqa: BLE001
        return SongResult(name, None, txt_path, len(images_sorted), len(tab.measures),
                          error=f"failed to write .gp5: {e}")
    return SongResult(name, gp5_path, txt_path, len(images_sorted), len(tab.measures),
                      error=(f"OCR errors on {len(ocr_errors)} page(s)" if ocr_errors else None))


def process_folder(
    input_folder: str | Path,
    output_dir: str | Path = "output",
    *,
    method: str = "vision",
    model: str = DEFAULT_MODEL,
    concurrency: int = DEFAULT_CONCURRENCY,
) -> list[SongResult]:
    """Process every song subfolder under ``input_folder``."""
    input_folder = Path(input_folder).resolve()
    output_dir = Path(output_dir).resolve()
    if not input_folder.is_dir():
        raise FileNotFoundError(f"Input folder not found: {input_folder}")

    songs = _song_dirs(input_folder)
    if not songs:
        print(f"No song subfolders with images found under: {input_folder}")
        return []

    results: list[SongResult] = []
    for idx, (name, song_dir) in enumerate(songs, 1):
        # Resume support: skip songs already converted.
        existing = output_dir / name / f"{name}.gp5"
        if existing.exists():
            print(f"[{idx}/{len(songs)}] skip (exists): {name}")
            results.append(SongResult(name, existing, existing.with_suffix(".txt"),
                                      0, 0, error=None))
            continue
        print(f"\n[{idx}/{len(songs)}] → Processing song: {name}  ({song_dir})")
        res = process_song(name, song_dir, output_dir,
                           method=method, model=model, concurrency=concurrency)
        if res.gp5_path:
            print(f"   ✓ {res.n_images} images → {res.n_measures} measures → {res.gp5_path}")
        else:
            print(f"   ✗ failed: {res.error}")
        results.append(res)

    ok = sum(1 for r in results if r.gp5_path)
    print(f"\nDone: {ok}/{len(results)} songs converted → {output_dir}")
    return results
