#!/usr/bin/env python3
"""Generate synthetic .sloppak test songs for slopsmith playback testing.

Uses lib/song.py's real serializers (arrangement_to_wire) so the on-disk
arrangement JSON matches what the loader expects exactly, and generates
synced sine-tone audio (stdlib wave -> WAV) so the highway notes line up
with audible sound. No copyrighted material — pure synthesis. (This
ffmpeg build lacks libvorbis, so stems are WAV — slopsmith serves .wav
stems fine and Electron plays them.)

Run from anywhere (lib/ resolves via the script's own location):
    python scripts/make_test_songs.py <out_dir>
"""
import json
import math
import os
import struct
import subprocess
import sys
import wave
from pathlib import Path

import yaml  # PyYAML — slopsmith already depends on it for manifest parsing

# Make lib/ importable regardless of CWD.
_HERE = Path(__file__).resolve().parent
_ROOT = _HERE.parent
sys.path.insert(0, str(_ROOT / "lib"))
from song import (Note, Chord, Anchor, Beat, Section, ChordTemplate,
                  Arrangement, Song, arrangement_to_wire)  # noqa: E402

# Standard tuning base MIDI notes per string (string 0 = low E2).
BASE_MIDI = [40, 45, 50, 55, 59, 64]  # E2 A2 D3 G3 B3 E4
SAMPLE_RATE = 44100


def midi_to_freq(m: int) -> float:
    return 440.0 * (2.0 ** ((m - 69) / 12.0))


def note_freq(string: int, fret: int, tuning: list[int]) -> float:
    t = tuning[string] if string < len(tuning) else 0
    return midi_to_freq(BASE_MIDI[string] + fret + t)


def write_wav(path: Path, samples: list[float]):
    """Write mono 16-bit PCM."""
    n = len(samples)
    with wave.open(str(path), "w") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SAMPLE_RATE)
        frames = bytearray()
        peak = max(1.0, max(abs(s) for s in samples))
        for s in samples:
            v = int(max(-1.0, min(1.0, s / peak)) * 32767)
            frames += struct.pack("<h", v)
        w.writeframes(bytes(frames))


def render_audio(path: Path, notes, chords, tuning, duration: float):
    """Sum sine bursts for each note; chord notes overlap. Writes WAV
    (pcm_s16le) — this ffmpeg build has no libvorbis/vorbis-encoder, but
    slopsmith serves .wav stems (audio/wav) and Electron plays them."""
    n_samples = int(duration * SAMPLE_RATE) + SAMPLE_RATE  # +1s tail
    buf = [0.0] * n_samples
    events = list(notes)
    for c in chords:
        events.extend(c.notes)
    for n in events:
        if n.string < 0 or n.string >= len(BASE_MIDI):
            continue
        f = note_freq(n.string, n.fret, tuning)
        start = n.time
        sus = n.sustain if n.sustain > 0 else 0.45
        i0 = int(start * SAMPLE_RATE)
        i1 = min(n_samples, int((start + sus) * SAMPLE_RATE))
        # attack/decay envelope (10ms) to avoid clicks
        atk = int(0.010 * SAMPLE_RATE)
        for i in range(i0, i1):
            rel = i - i0
            env = 0.35
            if rel < atk:
                env *= rel / atk
            elif (i1 - i) < atk:
                env *= (i1 - i) / atk
            buf[i] += env * math.sin(2 * math.pi * f * (i - i0) / SAMPLE_RATE)
    # path ends in .wav — write directly, no transcoding needed.
    write_wav(path, buf)


def beats_for(duration: float, bpm: float = 120.0, offset: float = 0.0):
    spb = 60.0 / bpm
    beats = []
    t = offset
    measure = 0
    beat_in_measure = 0
    while t < duration:
        beats.append(Beat(time=t, measure=(measure if beat_in_measure == 0 else -1)))
        beat_in_measure = (beat_in_measure + 1) % 4
        if beat_in_measure == 0:
            measure += 1
        t += spb
    return beats


def write_sloppak(out_dir: Path, song: Song, arrs: list[Arrangement]):
    """Write a .sloppak/ directory + audio. First arrangement carries beats/sections."""
    spath = out_dir / f"{_safe(song.title)}.sloppak"
    (spath / "arrangements").mkdir(parents=True, exist_ok=True)
    (spath / "stems").mkdir(exist_ok=True)

    # Attach song-level beats/sections to the first arrangement.
    arrs[0].notes  # noqa
    manifest_arrs = []
    for idx, arr in enumerate(arrs):
        if idx == 0:
            arr.notes  # ensure exists
        wire = arrangement_to_wire(arr)
        if idx == 0:
            wire["beats"] = [{"time": b.time, "measure": b.measure} for b in song.beats]
            wire["sections"] = [{"name": s.name, "number": s.number, "start_time": s.start_time}
                                for s in song.sections]
        af = spath / "arrangements" / f"{arr.name.lower()}.json"
        af.write_text(json.dumps(wire, separators=(",", ":")))
        manifest_arrs.append({
            "id": arr.name.lower(), "name": arr.name,
            "file": f"arrangements/{arr.name.lower()}.json",
            "tuning": arr.tuning, "capo": arr.capo,
        })

    # Audio (from lead arrangement notes; fall back to first arr). WAV — no
    # libvorbis in this ffmpeg build, but slopsmith serves .wav stems fine.
    audio = spath / "stems" / "full.wav"
    lead = arrs[0]
    render_audio(audio, lead.notes, lead.chords, lead.tuning, song.song_length)

    manifest = {
        "title": song.title, "artist": song.artist, "album": song.album,
        "year": song.year, "duration": song.song_length,
        "arrangements": manifest_arrs,
        "stems": [{"id": "full", "file": "stems/full.wav", "default": True}],
    }
    (spath / "manifest.yaml").write_text(yaml.safe_dump(manifest, sort_keys=False))
    print(f"  wrote {spath}  ({song.song_length:.0f}s, {len(arrs)} arr, "
          f"{sum(len(a.notes) for a in arrs)} notes)")


def _safe(s: str) -> str:
    return "".join(c if c.isalnum() or c in "-_" else "_" for c in s).strip("_").lower() or "song"


# ── Song definitions ──────────────────────────────────────────────────────

def song_open_strings():
    n = []
    t = 2.0
    for s in range(6):  # open notes up the strings
        n.append(Note(time=t, string=s, fret=0, sustain=0.8))
        t += 1.0
    for s in range(5, -1, -1):  # back down
        n.append(Note(time=t, string=s, fret=0, sustain=0.8))
        t += 1.0
    dur = t + 1.0
    arr = Arrangement(name="Lead", tuning=[0]*6, notes=n,
                      anchors=[Anchor(time=2.0, fret=0)])
    song = Song(title="Open String Warmup", artist="Slopsmith Test Tones",
                album="Synthetic", year=2026, song_length=dur,
                beats=beats_for(dur, 120, 2.0),
                sections=[Section(name="Verse", number=1, start_time=2.0)])
    return song, [arr]


def song_pentatonic():
    # E minor pentatonic: E G A B D E across fretboard, frets on strings.
    # string/fret pairs (roughly playable E minor pentatonic run)
    run = [(0, 0), (1, 2), (1, 4), (2, 2), (2, 4), (3, 2), (3, 4),
           (4, 3), (4, 5), (5, 3), (5, 5), (5, 3), (4, 3), (3, 2)]
    n = []
    t = 2.0
    for s, f in run:
        n.append(Note(time=t, string=s, fret=f, sustain=0.35, accent=(t < 4)))
        t += 0.4
    dur = t + 1.5
    arr = Arrangement(name="Lead", tuning=[0]*6, notes=n,
                      anchors=[Anchor(time=2.0, fret=2)])
    song = Song(title="E Minor Pentatonic Lick", artist="Slopsmith Test Tones",
                album="Synthetic", year=2026, song_length=dur,
                beats=beats_for(dur, 140, 2.0),
                sections=[Section(name="Lick", number=1, start_time=2.0)])
    return song, [arr]


def song_power_chords():
    # Drop D power chords: two-note chords (root + fifth), lead + bass.
    drop_d = [-2, 0, 0, 0, 0, 0]
    chords = []
    t = 2.0
    progression = [(0, 0), (1, 2), (2, 2), (3, 2)]  # move a shape up strings
    for _ in range(4):
        for s, f in progression:
            root = Note(time=t, string=s, fret=f, sustain=0.6)
            fifth = Note(time=t, string=s + 1, fret=f + 2, sustain=0.6)
            chords.append(Chord(time=t, chord_id=0, notes=[root, fifth]))
            t += 0.8
    dur = t + 1.5
    # A couple of templates
    tmpl = [ChordTemplate(name="P5", fingers=[1, 3, -1, -1, -1, -1],
                          frets=[0, 2, -1, -1, -1, -1])]
    lead = Arrangement(name="Lead", tuning=drop_d, notes=[], chords=chords,
                       chord_templates=tmpl, anchors=[Anchor(time=2.0, fret=2)])
    # Bass: single root notes following the chords
    bass_notes = []
    bt = 2.0
    for _ in range(4):
        for s, f in progression:
            bass_notes.append(Note(time=bt, string=0, fret=f, sustain=0.7))
            bt += 0.8
    bass = Arrangement(name="Bass", tuning=[-2, 0, 0, 0],
                       notes=bass_notes, anchors=[Anchor(time=2.0, fret=2)])
    song = Song(title="Power Chord Punch", artist="Slopsmith Test Tones",
                album="Synthetic", year=2026, song_length=dur,
                beats=beats_for(dur, 100, 2.0),
                sections=[Section(name="Riff", number=1, start_time=2.0)])
    return song, [lead, bass]


def main():
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("dlc-out")
    out.mkdir(parents=True, exist_ok=True)
    print(f"Generating synthetic sloppak songs into: {out}")
    for fn in (song_open_strings, song_pentatonic, song_power_chords):
        song, arrs = fn()
        write_sloppak(out, song, arrs)
    print("Done.")


if __name__ == "__main__":
    main()
