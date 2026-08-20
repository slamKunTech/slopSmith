"""Metadata repair for sloppak manifests (shared by server.py and scripts).

GP/RS conversion tools historically read GBK bytes as BIG5, producing
mojibake fields made of rare CJK glyphs. Every corruption pattern observed
so far is repaired by re-encoding BIG5 then decoding GBK:
    珔需藝 → 叶惠美, 扂竭疆 → 我很忙, 坋珨堎腔祊堊 → 十一月的萧邦,
    藹豌釱 → 魔杰座, 蔬鰍 → 江南, ▲匐僅諾潔◎ → 《八度空间》,
    勒びMr_J → 扒谱Mr_J

The marker gate matters: correct simplified-Chinese strings like '晴天'
also encode as BIG5, so repair is only attempted when the string contains
at least one marker glyph from the observed corruption set — glyphs that
never legitimately appear in simplified-Chinese metadata. '需'/'藝'/'蔬'/
'竭'/'疆'/'勒' are deliberately NOT markers: they are ordinary characters
that happen to appear inside corrupted strings, but their corrupted
partners (珔/坋/扂/鰍/び/...) already gate those strings.
"""

import re

# Rare glyphs only seen in corrupted metadata. ▲/◎ are the BIG5-mapped
# decorative corners that accompany mojibake album names (▲珔需藝◎).
_MOJIBAKE_MARKERS = frozenset("珔坋珨堎祊堊釱扂鰍藹豌婓び▲◎")

# GP conversion stems that carry no recoverable song name (s, 2ss, 4A ss,
# 5Ass, gtp.cn, www.gtp.cn, ...) plus 1-2 char ASCII scraps the guitarpro
# decoder leaves behind ('B', 'B1'). Used both as junk detection and as
# the "hide from library" predicate.
_JUNK_TITLE_RE = re.compile(r"^(s+|(?:\d+[a-z]?\s*s+)|gtp\.cn|www\.gtp\.cn|[a-z0-9]{1,2})$", re.IGNORECASE)

# Converter wrote the song name into artist and the gtp.cn credit line
# into title (e.g. title='deanhjy 扒谱！提供gtp.cn', artist='有没有人告诉
# 你（木吉他版）').
_SWAP_TITLE_RE = re.compile(r"扒谱|提供|gtp\.cn|www\.", re.IGNORECASE)

# Trailing converter hash from scripts/convert_gp_to_sloppak.py
# (md5 of the source path, 6 hex chars).
_HASH_SUFFIX_RE = re.compile(r"-[0-9a-f]{6}$", re.IGNORECASE)


def repair_text(value) -> str:
    """Repair one field corrupted by a GBK-as-BIG5 misread.

    Returns the input unchanged (stripped) when the marker gate does not
    fire or the roundtrip looks wrong."""
    s = value.strip() if isinstance(value, str) else ""
    if not s or not any(c in _MOJIBAKE_MARKERS for c in s):
        return s
    try:
        repaired = s.encode("big5").decode("gbk")
    except (UnicodeEncodeError, UnicodeDecodeError):
        return s
    # Sanity checks: must differ, be plausibly Chinese, and not still
    # contain marker glyphs (which would indicate a misfire or double
    # corruption).
    if not repaired or repaired == s:
        return s
    if not any("一" <= c <= "鿿" for c in repaired):
        return s
    if any(c in _MOJIBAKE_MARKERS for c in repaired):
        return s
    return repaired


def is_junk_title(title: str) -> bool:
    """True when the title is an unrecoverable GP conversion stem."""
    return bool(_JUNK_TITLE_RE.match(title or ""))


def clean_filename_stem(filename: str) -> str:
    """Filename minus '.sloppak' and the trailing converter hash."""
    name = filename[:-8] if filename.lower().endswith(".sloppak") else filename
    return _HASH_SUFFIX_RE.sub("", name).strip()


def repair_meta(meta: dict, filename: str) -> dict:
    """Normalize sloppak metadata for display. Mutates and returns meta.

    1. Gated GBK-as-BIG5 mojibake repair on title/artist/album.
    2. Swap title/artist when the converter wrote the song name into
       artist and the gtp.cn credit line into title.
    3. Fall back to the cleaned filename stem when title is empty or
       unrecoverable junk.
    4. Set meta['hidden'] when the resulting title is still junk —
       callers (the library scan) hide these rows. Callers that write
       manifests back to disk should pop 'hidden' first."""
    meta["title"] = repair_text(meta.get("title", ""))
    meta["artist"] = repair_text(meta.get("artist", ""))
    meta["album"] = repair_text(meta.get("album", ""))

    if _SWAP_TITLE_RE.search(meta["title"]) and meta["artist"] and meta["artist"] != "Unknown":
        meta["title"], meta["artist"] = meta["artist"], meta["title"]
        if _SWAP_TITLE_RE.search(meta["artist"]):
            meta["artist"] = ""  # the credit line is not an artist

    if not meta["title"] or is_junk_title(meta["title"]):
        meta["title"] = clean_filename_stem(filename)

    # Truncated-title heuristic: converters often write just the first word
    # of the source filename into the title ('Bli' while the file is
    # 'Bli《跳楼机》（变调迦老师）'). When the title is a bare ASCII scrap
    # and the cleaned filename stem is a strict extension of it, prefer the
    # stem — it carries the real name. CJK titles are never replaced here:
    # a swapped-in song name (e.g. '有没有人告诉你（木吉他版）') must not
    # absorb the credit parenthetical from the stem, and titles that merely
    # appear inside the stem ('轨迹' in '周杰伦[轨迹]-69a46b') are left
    # alone by the startswith requirement.
    if meta["title"] and not any(ord(c) > 127 for c in meta["title"]):
        stem = clean_filename_stem(filename)
        if stem.startswith(meta["title"]) and len(stem) > len(meta["title"]) + 1:
            meta["title"] = stem

    meta["hidden"] = is_junk_title(meta["title"])
    return meta
