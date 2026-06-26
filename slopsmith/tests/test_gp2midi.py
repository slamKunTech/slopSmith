"""Tests for lib/gp2midi.py soundfont discovery + install-hint helpers.

The interesting pure functions here are ``_find_soundfont`` (env var →
RESOURCESPATH glob → per-OS system paths) and the two ``*_install_hint``
helpers that emit platform-aware prose used in error messages. Fluidsynth
subprocess execution is out of scope; those tests would need the binary
on CI and add no pure-logic coverage.

See issue #11.
"""

import sys

import pytest

import gp2midi


# ── _find_soundfont ──────────────────────────────────────────────────────────

def test_find_soundfont_honours_env_var(tmp_path, monkeypatch):
    sf = tmp_path / "custom.sf2"
    sf.write_bytes(b"riff")  # content unchecked; only os.path.isfile matters
    monkeypatch.setenv("SLOPSMITH_SOUNDFONT", str(sf))
    monkeypatch.delenv("RESOURCESPATH", raising=False)

    assert gp2midi._find_soundfont() == str(sf)


def test_find_soundfont_env_var_skipped_when_file_missing(tmp_path, monkeypatch):
    # Env var set but path doesn't exist — fall through to later candidates.
    # Clear RESOURCESPATH too so the fall-through lands on system paths
    # (which will themselves be empty on the test runner → None).
    monkeypatch.setenv("SLOPSMITH_SOUNDFONT", str(tmp_path / "nonexistent.sf2"))
    monkeypatch.delenv("RESOURCESPATH", raising=False)
    # Override sys.platform to something unrecognised so no system paths apply.
    monkeypatch.setattr(gp2midi.sys, "platform", "unknown-os")

    assert gp2midi._find_soundfont() is None


def test_find_soundfont_picks_from_resourcespath(tmp_path, monkeypatch):
    sf_dir = tmp_path / "soundfonts"
    sf_dir.mkdir()
    (sf_dir / "GeneralUser-GS.sf2").write_bytes(b"riff")

    monkeypatch.delenv("SLOPSMITH_SOUNDFONT", raising=False)
    monkeypatch.setenv("RESOURCESPATH", str(tmp_path))

    assert gp2midi._find_soundfont() == str(sf_dir / "GeneralUser-GS.sf2")


def test_find_soundfont_env_var_wins_over_resourcespath(tmp_path, monkeypatch):
    # Two candidates available — env var takes precedence.
    bundled = tmp_path / "soundfonts" / "bundled.sf2"
    bundled.parent.mkdir()
    bundled.write_bytes(b"riff")

    override = tmp_path / "override.sf2"
    override.write_bytes(b"riff")

    monkeypatch.setenv("SLOPSMITH_SOUNDFONT", str(override))
    monkeypatch.setenv("RESOURCESPATH", str(tmp_path))

    assert gp2midi._find_soundfont() == str(override)


def test_find_soundfont_returns_none_when_nothing_exists(tmp_path, monkeypatch):
    monkeypatch.delenv("SLOPSMITH_SOUNDFONT", raising=False)
    monkeypatch.setenv("RESOURCESPATH", str(tmp_path))  # empty dir, no sf2
    monkeypatch.setattr(gp2midi.sys, "platform", "unknown-os")

    assert gp2midi._find_soundfont() is None


def test_find_soundfont_warns_on_invalid_env_var(tmp_path, monkeypatch, capsys):
    # Env var set to a non-existent path: warn on stderr so the misconfig is
    # visible in the server log, then fall through.
    monkeypatch.setenv("SLOPSMITH_SOUNDFONT", str(tmp_path / "nope.sf2"))
    monkeypatch.delenv("RESOURCESPATH", raising=False)
    monkeypatch.setattr(gp2midi.sys, "platform", "unknown-os")

    result = gp2midi._find_soundfont()
    assert result is None

    captured = capsys.readouterr()
    assert "SLOPSMITH_SOUNDFONT" in captured.err
    assert "does not exist" in captured.err


@pytest.mark.parametrize("platform,target", [
    ("linux",  "/usr/share/soundfonts/FluidR3_GM.sf2"),
    ("linux",  "/usr/share/sounds/sf2/FluidR3_GM.sf2"),
    ("darwin", "/opt/homebrew/share/sounds/sf2/FluidR3_GM.sf2"),
    ("darwin", "/usr/local/share/sounds/sf2/FluidR3_GM.sf2"),
])
def test_find_soundfont_picks_up_system_paths_per_platform(platform, target, monkeypatch):
    # Simulate "only the target file exists" on a given platform. The fake
    # isfile must return False for the env var and resourcepath fall-throughs
    # as well, so we guard with an exact match.
    monkeypatch.delenv("SLOPSMITH_SOUNDFONT", raising=False)
    monkeypatch.delenv("RESOURCESPATH", raising=False)
    monkeypatch.setattr(gp2midi.sys, "platform", platform)
    monkeypatch.setattr(gp2midi.os.path, "isfile", lambda p: p == target)

    assert gp2midi._find_soundfont() == target


def test_render_midi_to_audio_surfaces_fluidsynth_not_found(tmp_path, monkeypatch):
    # If fluidsynth isn't on PATH, the subprocess call raises
    # FileNotFoundError. The wrapper must convert that into a clear
    # "fluidsynth not found" RuntimeError rather than bubbling the raw OS
    # error up to the caller.
    sf = tmp_path / "stub.sf2"
    sf.write_bytes(b"riff")
    monkeypatch.setenv("SLOPSMITH_SOUNDFONT", str(sf))

    def fake_run(*args, **kwargs):
        raise FileNotFoundError("[Errno 2] No such file or directory: 'fluidsynth'")
    monkeypatch.setattr(gp2midi.subprocess, "run", fake_run)

    with pytest.raises(RuntimeError) as exc_info:
        gp2midi.render_midi_to_audio(str(tmp_path / "in.mid"), str(tmp_path / "out"))
    assert "fluidsynth not found" in str(exc_info.value)


# ── _soundfont_install_hint ──────────────────────────────────────────────────
# The hint text itself isn't content-tested line-by-line (it'd be brittle
# to prose edits). We just assert platform keyed phrases appear so a
# Windows user never sees `pacman` and a Linux user doesn't see `brew`.

@pytest.mark.parametrize("platform,must_contain,must_not_contain", [
    ("linux",   ["pacman", "apt", "dnf"],                          ["homebrew", "FluidSynth.exe"]),
    ("linux2",  ["pacman", "apt", "dnf"],                          ["homebrew"]),
    ("darwin",  ["homebrew", "SLOPSMITH_SOUNDFONT"],               ["pacman", "apt"]),
    ("win32",   ["SLOPSMITH_SOUNDFONT", "APPDATA", "Slopsmith"],   ["pacman", "homebrew", "apt"]),
])
def test_soundfont_install_hint_is_platform_aware(platform, must_contain, must_not_contain, monkeypatch):
    monkeypatch.setattr(gp2midi.sys, "platform", platform)
    hint = gp2midi._soundfont_install_hint()
    for token in must_contain:
        assert token in hint, f"expected {token!r} in hint for {platform}, got: {hint!r}"
    for token in must_not_contain:
        assert token not in hint, f"didn't expect {token!r} in hint for {platform}, got: {hint!r}"


def test_soundfont_install_hint_unknown_platform_falls_back_to_env_var_instruction(monkeypatch):
    monkeypatch.setattr(gp2midi.sys, "platform", "freebsd14")
    hint = gp2midi._soundfont_install_hint()
    assert "SLOPSMITH_SOUNDFONT" in hint


# ── _fluidsynth_install_hint ─────────────────────────────────────────────────

@pytest.mark.parametrize("platform,must_contain,must_not_contain", [
    ("linux",   ["pacman", "apt", "dnf"],         ["brew"]),
    ("darwin",  ["brew"],                          ["pacman", "apt"]),
    ("win32",   ["fluidsynth.exe", "PATH"],        ["pacman", "brew", "apt"]),
])
def test_fluidsynth_install_hint_is_platform_aware(platform, must_contain, must_not_contain, monkeypatch):
    monkeypatch.setattr(gp2midi.sys, "platform", platform)
    hint = gp2midi._fluidsynth_install_hint()
    for token in must_contain:
        assert token in hint, f"expected {token!r} in hint for {platform}, got: {hint!r}"
    for token in must_not_contain:
        assert token not in hint, f"didn't expect {token!r} in hint for {platform}, got: {hint!r}"
