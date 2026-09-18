"""Transcribe engraved guitar-tab images to ASCII tab using Claude vision.

Tesseract OCR cannot read engraved staves (staff lines + fret numbers in a
six-line tab staff) — it produces garbage. This module replaces OCR with a
vision LLM (Claude) that reads the tab stave directly and emits clean ASCII
tab, which the existing :mod:`ascii_parser` + :mod:`song_builder` then turn
into a ``.gp5``.

The LLM is asked to output **only** the six-string ASCII tab — one 6-line
block per tab system, barline-delimited — in the exact format
:func:`pic2guitarpro.ascii_parser.parse_tab` expects.
"""

from __future__ import annotations

import base64
import io
import os
from pathlib import Path

import anthropic
from PIL import Image

# Default model for vision transcription. The project runs against the
# DashScope Anthropic-compatible relay, where only some models accept image
# input: qwen3.6-plus (Qwen-VL) sees images; glm-5.2 does NOT (image is
# silently ignored). Override with --model or ANTHROPIC_MODEL.
DEFAULT_MODEL = os.environ.get("PIC2GP_MODEL", "qwen3.6-plus")

# Cap the image's long edge before sending. The source PNGs are huge
# (5800×8200+); downsampling to 800px cuts input tokens ~5× and per-call time
# ~2-3× with no loss of tab-reading accuracy (tab staves are simple glyphs).
IMAGE_MAX_EDGE = 800

SYSTEM_PROMPT = """\
You are an expert guitar tablature transcriber. You are given an image of \
printed/engraved guitar tablature (six-line tab staves, possibly with a \
standard-music-notation staff above, chord names, lyrics, and header text).

Your job: transcribe ONLY the six-line TAB staves into clean ASCII tab. \
Ignore the standard notation staff, chord-name labels, lyrics, titles, and \
any other text — output nothing but the tab.

OUTPUT FORMAT (exact):
- Each tab system is six lines, top to bottom, labelled e B G D A E \
  (high-e string on top, low-E on the bottom). Each line starts with the \
  letter, then `|`, then the fret digits, then a closing `|`.
- Fret numbers are decimal digits 0-24. A two-digit fret (10-24) is written \
  as two adjacent digits with no dash between them.
- Use `-` (dash) as the rail/spacer between fret numbers within a measure.
- Use `|` as the barline between measures. Every line of a system must have \
  barlines at the same columns so the six strings stay aligned.
- Notes struck simultaneously (a chord) must be at the same character column \
  across the six strings.
- Leave a blank line between consecutive tab systems.
- Output ONLY the ASCII tab. No prose, no explanations, no chord names, no \
  markdown fences.

If a part of the image is unreadable, transcribe what you can confidently \
read and omit the unclear bars rather than guessing frets.

Example output:
e|-----------------|
B|-----------------|
G|-----------------|
D|--------5--------|
A|------5----5-----|
E|---3----------3--|
"""

USER_INSTRUCTION = (
    "Transcribe every guitar tab stave in this image to ASCII tab, following "
    "the format in the system prompt. Output only the ASCII tab."
)

# Cap output tokens: ASCII tab is verbose but a single page rarely exceeds ~4K
# tokens. Stream to stay well under SDK HTTP timeouts.
MAX_TOKENS = 16000


def _encode_image(image_path: Path) -> tuple[str, str]:
    """Return ``(base64_data, media_type)`` for an image, downscaled to
    ``IMAGE_MAX_EDGE`` on the long edge and re-encoded as JPEG to cut payload."""
    im = Image.open(image_path).convert("RGB")
    im.thumbnail((IMAGE_MAX_EDGE, IMAGE_MAX_EDGE))
    buf = io.BytesIO()
    im.save(buf, format="JPEG", quality=85)
    return base64.standard_b64encode(buf.getvalue()).decode("utf-8"), "image/jpeg"


def transcribe_image(
    image_path: Path | str,
    *,
    model: str = DEFAULT_MODEL,
    client: anthropic.Anthropic | None = None,
) -> str:
    """Transcribe one tab image to ASCII tab text via a vision LLM.

    Resolves credentials the same way the SDK always does (``ANTHROPIC_API_KEY``,
    an ``ant auth login`` profile, etc.) — pass a ``client`` only to override.

    Uses non-streaming ``messages.create`` (the relay's SSE streaming did not
    reliably return content for non-Claude models). Adaptive thinking is sent
    only for Claude models; other models (Qwen/GLM via relay) ignore or reject it.
    """
    image_path = Path(image_path)
    data, media_type = _encode_image(image_path)
    client = client or anthropic.Anthropic()

    kwargs: dict = {
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": SYSTEM_PROMPT,
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "image",
                        "source": {"type": "base64", "media_type": media_type, "data": data},
                    },
                    {"type": "text", "text": USER_INSTRUCTION},
                ],
            }
        ],
    }
    if model.startswith("claude-"):
        kwargs["thinking"] = {"type": "adaptive"}

    message = client.messages.create(**kwargs)

    if message.stop_reason == "refusal":
        return ""
    return "".join(block.text for block in message.content if block.type == "text")
