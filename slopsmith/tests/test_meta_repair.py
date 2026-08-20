"""Tests for lib/meta_repair.py — GBK-as-BIG5 mojibake repair, title/artist
swap, junk-title detection and the hidden flag used by the library scan.

The expected outputs below were reverse-engineered from the user's real
manifest.yaml files: the corrupted strings are original GBK bytes that a
conversion tool decoded as BIG5, so `encode('big5').decode('gbk')`
recovers the true text (album names like 叶惠美 / 我很忙 / 十一月的萧邦).
"""

from lib import meta_repair


REPAIR_CASES = [
    ("珔需藝", "叶惠美"),
    ("扂竭疆", "我很忙"),
    ("坋珨堎腔祊堊", "十一月的萧邦"),
    ("<<坋珨堎腔祊堊>>", "<<十一月的萧邦>>"),
    ("藹豌釱", "魔杰座"),
    ("蔬鰍", "江南"),
    ("▲匐僅諾潔◎", "《八度空间》"),
    ("勒びMr_J", "扒谱Mr_J"),
    ("婓梗揭", "在别处"),
]


def test_repair_text_recovers_mojibake():
    for corrupted, expected in REPAIR_CASES:
        assert meta_repair.repair_text(corrupted) == expected, corrupted


def test_repair_text_leaves_clean_strings_untouched():
    clean = [
        "晴天",                      # encodes as BIG5 but has no marker glyphs
        "周杰伦 安静",
        "欧若拉（乐队全谱）",
        "《半岛铁盒》完整版",
        "www.gtp.cn GTP中文娱乐网搜集  吉它手的好去处",
        "",
    ]
    for s in clean:
        assert meta_repair.repair_text(s) == s.strip()


def test_repair_text_strips_trailing_whitespace():
    assert meta_repair.repair_text("4A 同桌的你 ") == "4A 同桌的你"


def test_swap_title_and_artist():
    meta = meta_repair.repair_meta(
        {"title": "deanhjy 扒谱！提供gtp.cn", "artist": "有没有人告诉你（木吉他版）", "album": ""},
        "有没有人告诉你（木吉他版）(deanhjy 扒谱！提供gtp.cn)-488d00.sloppak",
    )
    assert meta["title"] == "有没有人告诉你（木吉他版）"
    assert meta["artist"] == ""  # the credit line is not an artist
    assert not meta["hidden"]


def test_empty_title_falls_back_to_cleaned_filename():
    meta = meta_repair.repair_meta({"title": "", "artist": "Unknown", "album": ""},
                                   "晴天-437940.sloppak")
    assert meta["title"] == "晴天"
    assert not meta["hidden"]


def test_junk_title_stays_junk_and_hides():
    meta = meta_repair.repair_meta({"title": "2ss", "artist": "Unknown", "album": ""},
                                   "2ss-17db79.sloppak")
    assert meta["title"] == "2ss"
    assert meta["hidden"]

    meta = meta_repair.repair_meta({"title": "s", "artist": "Unknown", "album": ""},
                                   "s-6b8cd4.sloppak")
    assert meta["title"] == "s"
    assert meta["hidden"]

    meta = meta_repair.repair_meta({"title": "www.gtp.cn", "artist": "Unknown", "album": ""},
                                   "www.gtp.cn-5cedbf.sloppak")
    assert meta["hidden"]


def test_prefixed_junk_with_real_name_is_not_hidden():
    # '4A' prefix + real song name is recoverable-looking; only pure GP
    # stems count as junk.
    meta = meta_repair.repair_meta({"title": "4A 同桌的你", "artist": "Unknown", "album": ""},
                                   "4A 同桌的你 -96ee44.sloppak")
    assert meta["title"] == "4A 同桌的你"
    assert not meta["hidden"]


def test_album_mojibake_repaired_in_meta():
    meta = meta_repair.repair_meta({"title": "晴天", "artist": "Unknown", "album": "▲珔需藝◎"},
                                   "晴天-437940.sloppak")
    assert meta["album"] == "《叶惠美》"
    assert not meta["hidden"]


def test_single_char_title_is_junk_and_falls_back_to_stem():
    meta = meta_repair.repair_meta({"title": "B", "artist": "Unknown", "album": ""},
                                   "B哥《热河路》（变调迦老师）-51433c.sloppak")
    assert meta["title"] == "B哥《热河路》（变调迦老师）"
    assert not meta["hidden"]


def test_truncated_title_prefers_longer_stem():
    meta = meta_repair.repair_meta({"title": "Bli", "artist": "Unknown", "album": ""},
                                   "Bli《跳楼机》（变调迦老师）-190893.sloppak")
    assert meta["title"] == "Bli《跳楼机》（变调迦老师）"

    meta = meta_repair.repair_meta({"title": "Dear John-", "artist": "Unknown", "album": ""},
                                   "Dear John-比莉-5ec86a.sloppak")
    assert meta["title"] == "Dear John-比莉"


def test_title_inside_stem_is_not_replaced():
    meta = meta_repair.repair_meta({"title": "轨迹", "artist": "周杰伦", "album": ""},
                                   "周杰伦[轨迹]-69a46b.sloppak")
    assert meta["title"] == "轨迹"
