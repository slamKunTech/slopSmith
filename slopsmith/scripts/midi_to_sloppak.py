#!/usr/bin/env python3
"""Convert a MIDI file to a sloppak package.

MIDI carries notation only, so audio is synthesized: the file is rendered
through fluidsynth (same path as the GP converter) into stems/full.{wav,ogg}.
If the render fails (no fluidsynth/soundfont), the sloppak is still written
with arrangements but no stems, and the player will report "no playable stems".

--melody-only drops left-hand accompaniment (bass/chords) and keeps only the
right-hand melody, in both the arrangements and the audio render. By default
it auto-selects the highest-pitch MIDI track(s); pass --split-pitch N instead
to filter at note level (keep notes >= N).

--guitar maps the notes onto standard EADGBe string/fret positions with
adjacent fingerings (dynamic programming over note candidates) instead of
the 24-semitone-per-lane piano encoding, so the highway shows a compact
guitar tab instead of a wide fret spread.
"""

import sys
import json
import tempfile
import zipfile
from pathlib import Path

# Add parent to path to import lib modules
sys.path.insert(0, str(Path(__file__).parent.parent))

import mido
import yaml
from lib.midi_import import list_midi_tracks, convert_midi_track_to_keys_wire
from lib.gp2midi import render_midi_to_audio


def _beats_for(duration: float, bpm: float = 120.0) -> list[dict]:
    """Synthesized beat grid (measure-marked) when the wire lacks beats."""
    spb = 60.0 / bpm
    out = []
    t = 0.0
    measure = 0
    beat_in = 0
    while t < duration:
        out.append({"time": round(t, 3), "measure": (measure if beat_in == 0 else -1)})
        beat_in = (beat_in + 1) % 4
        if beat_in == 0:
            measure += 1
        t += spb
    return out


def _pick_right_hand_tracks(midi_path: str, merge_window: float = 8.0) -> list[int]:
    """Indices of the note-bearing MIDI tracks that make up the right hand.

    Heuristic: the right hand is the track with the highest mean pitch;
    other note-bearing tracks within `merge_window` semitones of it are
    kept too (melodies split across tracks), the rest (left-hand bass /
    chords) are dropped. Returns raw MidiFile track indices, ascending."""
    midi = mido.MidiFile(midi_path)
    stats = []
    for i, track in enumerate(midi.tracks):
        pitches = [int(m.note) for m in track
                   if m.type == "note_on" and int(getattr(m, "velocity", 0)) > 0]
        if pitches:
            stats.append((i, sum(pitches) / len(pitches)))
    if not stats:
        return []
    top = max(mean for _i, mean in stats)
    return sorted(i for i, mean in stats if top - mean <= merge_window)


def _filter_midi_tracks(midi_path: str, keep_tracks: set[int]) -> str:
    """Write a copy of the MIDI keeping only `keep_tracks` plus meta-only
    tracks (tempo/title — required for correct playback). Returns the new
    file path (caller deletes it)."""
    midi = mido.MidiFile(midi_path)
    out = mido.MidiFile(ticks_per_beat=midi.ticks_per_beat, type=midi.type)
    for i, track in enumerate(midi.tracks):
        if i in keep_tracks:
            out.tracks.append(track)
        else:
            has_notes = any(
                m.type == "note_on" and int(getattr(m, "velocity", 0)) > 0
                for m in track
            )
            if not has_notes:
                out.tracks.append(track)  # conductor/meta track
    out_path = tempfile.mktemp(suffix=".mid", prefix="rs_melody_")
    out.save(out_path)
    return out_path


def _filter_midi_by_pitch(midi_path: str, split: int) -> str:
    """Write a copy of the MIDI with note events below `split` removed.
    Tempo/meta events are preserved. Returns the new file path (caller
    deletes it)."""
    midi = mido.MidiFile(midi_path)
    out = mido.MidiFile(ticks_per_beat=midi.ticks_per_beat, type=midi.type)
    for track in midi.tracks:
        new = mido.MidiTrack()
        for msg in track:
            if msg.type in ("note_on", "note_off") and int(msg.note) < split:
                continue
            new.append(msg)
        out.tracks.append(new)
    out_path = tempfile.mktemp(suffix=".mid", prefix="rs_melody_")
    out.save(out_path)
    return out_path


# Standard EADGBe open-string pitches, string index 0 = low E (RS convention).
OPEN_STRINGS = [40, 45, 50, 55, 59, 64]
MAX_FRET = 22


def _map_notes_to_guitar(notes: list[dict]) -> tuple[list[dict], int]:
    """Re-encode keys-format notes (string = pitch//24) onto standard
    EADGBe positions with adjacent fingerings.

    Each note gets candidate (string, fret) positions; dynamic programming
    over the note sequence minimizes |Δfret| + 2·|Δstring| between successive
    notes (plus a tiny per-fret bias so the line prefers lower frets), which
    yields a compact, playable guitar line. Notes with no position inside
    [0, MAX_FRET] are dropped.

    Returns (remapped notes, number dropped)."""
    # Forward DP: per kept note, a list of (total_cost, s, f, prev_state_idx).
    states: list[list[tuple[float, int, int, int | None]]] = []
    kept: list[int] = []
    for idx, n in enumerate(notes):
        pitch = n["s"] * 24 + n["f"]
        cands = [(s, pitch - OPEN_STRINGS[s]) for s in range(len(OPEN_STRINGS))
                 if 0 <= pitch - OPEN_STRINGS[s] <= MAX_FRET]
        if not cands:
            continue
        if not states:
            new_states = [(0.02 * f, s, f, None) for s, f in cands]
        else:
            prev = states[-1]
            new_states = []
            for s2, f2 in cands:
                cost, pi = min(
                    (c1 + abs(f2 - f1) + 2 * abs(s2 - s1) + 0.02 * f2, pidx)
                    for pidx, (c1, s1, f1, _prev) in enumerate(prev)
                )
                new_states.append((cost, s2, f2, pi))
        states.append(new_states)
        kept.append(idx)

    if not states:
        return [], len(notes)

    # Backtrack from the cheapest final state.
    final = min(range(len(states[-1])), key=lambda i: states[-1][i][0])
    path: list[tuple[int, int]] = []
    si = final
    for k in range(len(states) - 1, -1, -1):
        _cost, s, f, prev = states[k][si]
        path.append((s, f))
        si = prev if prev is not None else 0
    path.reverse()

    out = [dict(notes[i]) for i in kept]
    for k, (s, f) in enumerate(path):
        out[k]["s"] = s
        out[k]["f"] = f
    return out, len(notes) - len(kept)


def midi_to_sloppak(midi_path: str, out_path: str,
                    melody_only: bool = False,
                    split_pitch: int | None = None,
                    guitar: bool = False) -> None:
    """Convert a MIDI file to a sloppak package.

    melody_only: drop left-hand accompaniment, keep only the right-hand
    melody. Without split_pitch, selects the highest-pitch track(s);
    with split_pitch, filters notes below that pitch in every track.
    guitar: map notes onto standard EADGBe string/fret positions with
    adjacent fingerings instead of the piano-style lanes.
    """
    midi_file = Path(midi_path)
    out_file = Path(out_path)

    if not midi_file.exists():
        raise FileNotFoundError(f"MIDI file not found: {midi_path}")

    split = split_pitch
    right_hand: list[int] | None = None
    if melody_only and split is None:
        right_hand = _pick_right_hand_tracks(str(midi_file))
        if not right_hand:
            raise RuntimeError("No note-bearing tracks found for --melody-only")
        print(f"Melody-only: keeping track(s) {right_hand} (right hand)", flush=True)
    elif melody_only:
        print(f"Melody-only: keeping notes >= MIDI pitch {split}", flush=True)

    print(f"Reading MIDI file: {midi_file.name}")
    tracks = list_midi_tracks(str(midi_file))

    if not tracks:
        raise RuntimeError("No playable tracks found in MIDI file")

    print(f"Found {len(tracks)} track(s):")
    for i, track in enumerate(tracks):
        print(f"  {i+1}. {track['name']} - {track['notes']} notes" +
              (" [Piano]" if track['is_piano'] else ""))

    # Create working directory
    with tempfile.TemporaryDirectory(prefix="midi2sloppak_") as td:
        work_dir = Path(td)
        arr_dir = work_dir / "arrangements"
        arr_dir.mkdir(parents=True, exist_ok=True)

        # Convert each track to an arrangement
        used_ids = set()
        arr_manifest = []
        wire_arrs = []

        for i, track in enumerate(tracks):
            track_idx = track['index']
            channel_filter = track.get('channel_filter')

            print(f"Converting track {i+1}/{len(tracks)}: {track['name']}")

            if right_hand is not None and track_idx not in right_hand:
                print(f"  → left hand (bass/chords), skipped", flush=True)
                continue

            # Generate unique arrangement ID
            base_id = track['name'].lower().replace(' ', '_').replace('-', '_')
            base_id = ''.join(c for c in base_id if c.isalnum() or c == '_').strip('_')
            base_id = base_id or f"track_{i+1}"

            arr_id = base_id
            counter = 2
            while arr_id in used_ids:
                arr_id = f"{base_id}_{counter}"
                counter += 1
            used_ids.add(arr_id)

            # Convert the track
            wire = convert_midi_track_to_keys_wire(
                str(midi_file),
                track_idx,
                audio_offset=0.0,
                name=track['name'],
                channel_filter=channel_filter
            )

            if split is not None:
                wire["notes"] = [n for n in wire["notes"]
                                 if n["s"] * 24 + n["f"] >= split]
                if not wire["notes"]:
                    print(f"  → {track['name']}: no notes after melody filter, skipped",
                          flush=True)
                    continue

            if guitar:
                wire["notes"], dropped = _map_notes_to_guitar(wire["notes"])
                wire["encoding"] = "guitar"
                if dropped:
                    print(f"  → {track['name']}: {dropped} notes outside guitar "
                          f"range dropped", flush=True)
                if not wire["notes"]:
                    print(f"  → {track['name']}: no playable guitar notes, skipped",
                          flush=True)
                    continue

            wire_arrs.append((arr_id, wire))

            arr_manifest.append({
                "id": arr_id,
                "name": track['name'],
                "file": f"arrangements/{arr_id}.json",
                "tuning": wire['tuning'],
                "capo": wire['capo'],
            })

            print(f"  → {arr_id}.json ({len(wire['notes'])} notes)")

        if not wire_arrs:
            raise RuntimeError("No notes remain after melody filter")

        # Song duration: furthest note end (t + sus) across arrangements.
        max_end = max(
            (n["t"] + n["sus"] for _aid, w in wire_arrs for n in w["notes"]),
            default=0.0,
        )
        duration = float(max_end + 2.0)

        # Beats/sections fallback on the first arrangement — the player uses
        # them for the beat grid / section labels.
        if wire_arrs:
            first = wire_arrs[0][1]
            if not first.get("beats"):
                first["beats"] = _beats_for(duration, 120.0)
            if not first.get("sections"):
                first["sections"] = [{"name": "Intro", "number": 1, "start_time": 0.0}]

        # Write arrangement JSONs
        for arr_id, wire in wire_arrs:
            arr_file = arr_dir / f"{arr_id}.json"
            arr_file.write_text(
                json.dumps(wire, separators=(",", ":")),
                encoding="utf-8"
            )

        # Audio: MIDI carries no audio, so synthesize a render via fluidsynth.
        # Without this the sloppak has no playable stems and the player shows
        # "Audio unavailable". In melody-only mode render the filtered copy so
        # the audio matches the kept notes.
        stems = []
        filtered_midi = None
        try:
            stems_dir = work_dir / "stems"
            stems_dir.mkdir(exist_ok=True)
            render_src = str(midi_file)
            if melody_only:
                if right_hand is not None:
                    filtered_midi = Path(_filter_midi_tracks(str(midi_file),
                                                             set(right_hand)))
                else:
                    filtered_midi = Path(_filter_midi_by_pitch(str(midi_file), split))
                render_src = str(filtered_midi)
            audio_path = render_midi_to_audio(render_src, str(stems_dir / "full"))
            stems = [{"id": "full", "file": f"stems/{Path(audio_path).name}", "default": True}]
        except Exception as e:
            print(f"! Audio render failed: {e} (sloppak written without audio)", flush=True)
        finally:
            if filtered_midi is not None:
                filtered_midi.unlink(missing_ok=True)

        # Create manifest
        title = midi_file.stem
        manifest = {
            "title": title,
            "artist": "",
            "album": "",
            "year": 0,
            "duration": duration,
            "stems": stems,
            "arrangements": arr_manifest,
        }

        manifest_file = work_dir / "manifest.yaml"
        manifest_file.write_text(
            yaml.safe_dump(manifest, sort_keys=False, allow_unicode=True),
            encoding="utf-8"
        )

        # Create README explaining the audio source
        readme = work_dir / "README.txt"
        if stems:
            readme.write_text(
                "This sloppak was generated from a MIDI file.\n"
                "Audio is a synthesized MIDI render (fluidsynth), not an original recording.\n",
                encoding="utf-8"
            )
        else:
            readme.write_text(
                "This sloppak was generated from a MIDI file.\n"
                "It contains note arrangements but no audio stems "
                "(audio rendering failed — see conversion log).\n"
                "You can add audio in the slopsmith editor.\n",
                encoding="utf-8"
            )

        # Package as zip
        print(f"Creating sloppak: {out_file.name}")
        out_file.parent.mkdir(parents=True, exist_ok=True)

        with zipfile.ZipFile(str(out_file), 'w', zipfile.ZIP_DEFLATED) as zf:
            for file in work_dir.rglob("*"):
                if file.is_file():
                    zf.write(file, file.relative_to(work_dir).as_posix())

        print(f"✓ Conversion complete: {out_file}")
        print(f"  Size: {out_file.stat().st_size / 1024:.1f} KB")


if __name__ == "__main__":
    import argparse
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("input", help="input .mid file")
    ap.add_argument("output", help="output .sloppak file")
    ap.add_argument("--melody-only", action="store_true",
                    help="drop left-hand accompaniment (bass/chords), keep only "
                         "the right-hand melody; auto-selects the highest-pitch "
                         "track(s), or use --split-pitch for a note-level threshold")
    ap.add_argument("--split-pitch", type=int, default=None,
                    help="with --melody-only: keep notes >= N instead of selecting tracks")
    ap.add_argument("--guitar", action="store_true",
                    help="map notes onto standard EADGBe string/fret positions "
                         "(adjacent fingerings) instead of piano-style lanes")
    args = ap.parse_args()

    try:
        midi_to_sloppak(args.input, args.output,
                        melody_only=args.melody_only,
                        split_pitch=args.split_pitch,
                        guitar=args.guitar)
    except Exception as e:
        print(f"Error: {e}", file=sys.stderr)
        sys.exit(1)
