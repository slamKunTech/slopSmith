"""CLI entry point: ``pic2guitarpro <input_folder> [-o output_dir]``."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from . import __version__
from .pipeline import process_folder
from .vision_transcriber import DEFAULT_MODEL


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="pic2guitarpro",
        description=(
            "Convert a folder of guitar tablature images into Guitar Pro (.gp5) "
            "scores. Each subfolder is one song (named after the subfolder); "
            "its images are ordered by Roman-numeral suffix."
        ),
    )
    parser.add_argument("input", help="Input folder containing per-song subfolders of tab images.")
    parser.add_argument(
        "-o", "--output",
        default="output",
        help="Output directory for .gp5 files (default: ./output).",
    )
    parser.add_argument(
        "-t", "--transcriber",
        choices=["vision", "tesseract"],
        default="vision",
        help="Image→ASCII-tab method. 'vision' (default) uses Claude vision and "
             "works on engraved staves; 'tesseract' only works on ASCII-tab images.",
    )
    parser.add_argument(
        "-m", "--model",
        default=DEFAULT_MODEL,
        help=f"Vision model (default: {DEFAULT_MODEL}). On the DashScope relay, "
             "qwen3.6-plus sees images; glm-5.2 does NOT.",
    )
    parser.add_argument(
        "-c", "--concurrency",
        type=int,
        default=8,
        help="Parallel image-transcription workers (default: 8).",
    )
    parser.add_argument("--version", action="version", version=f"%(prog)s {__version__}")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    input_folder = Path(args.input)
    output_dir = Path(args.output)
    try:
        results = process_folder(
            input_folder, output_dir,
            method=args.transcriber, model=args.model, concurrency=args.concurrency,
        )
    except FileNotFoundError as e:
        print(f"Error: {e}", file=sys.stderr)
        return 1
    return 0 if any(r.gp5_path for r in results) else 1


if __name__ == "__main__":
    sys.exit(main())
