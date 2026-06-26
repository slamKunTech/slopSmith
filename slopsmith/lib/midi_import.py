"""MIDI file import — list tracks and convert a track to a Keys arrangement.

Mirrors the shape of lib/gp2rs.py so the editor's track-picker UI can use
the same flow for both GP and MIDI files. Drum tracks (GM channel 9) are
filtered out of the listing entirely — the keys-import converter
unconditionally skips channel-9 events so a drums entry would always yield
an empty arrangement, and there is no MIDI drum-import flow today.
"""

from __future__ import annotations

from bisect import bisect_right
from collections import deque

import mido


# General MIDI piano-family programs (0-7) plus chromatic percussion + organ.
# Used to flag obvious keyboard tracks for the picker UI.
_KEY_PROGRAMS = set(range(0, 24))
_KEYBOARD_NAME_HINTS = (
    "piano", "keys", "keyboard", "synth", "organ", "rhodes",
    "harpsichord", "clavinet", "wurlitzer", "ep ", "epiano",
)


def list_midi_tracks(midi_path: str) -> list[dict]:
    """Return a list of track descriptors suitable for the picker UI.

    Format-0 MIDI files store every channel in a single track; if we just
    enumerated `midi.tracks` we'd produce one picker entry that merged
    every part into a single Keys arrangement. For format-0 only, split
    that single track into one virtual entry per non-drum channel so the
    user can isolate the piano part.

    Type-1 (parallel tracks) and type-2 (independent sequences) keep their
    one-entry-per-track shape: their tracks already represent the parts
    the author intended, and a track that uses e.g. LH/RH on separate
    channels would otherwise lose half its notes when the user picked
    just one of the split entries with no way to recover the merged form.

    Drum (channel-9) channels are dropped from the listing entirely —
    the keys-import converter unconditionally skips channel-9 events,
    so a drums entry would yield an empty arrangement, and there's no
    MIDI drum-import flow today.

    Each item: {index, name, instrument, notes, channel, is_piano, is_drums,
    channel_filter}. For split entries `channel_filter` is set; for
    unsplit entries it's None.
    """
    midi = mido.MidiFile(midi_path)
    tracks: list[dict] = []
    midi_type = getattr(midi, "type", 1)
    # Only format-0 collapses every part into one track and therefore
    # benefits from per-channel splitting. Type-1/2 tracks already
    # represent author-defined parts.
    split_format = (midi_type == 0)

    for i, track in enumerate(midi.tracks):
        name = ""
        # Per-channel stats, populated by walking the track once.
        per_channel: dict[int, dict] = {}

        for msg in track:
            if msg.type == "track_name" and not name:
                name = msg.name or ""
            elif msg.type == "program_change":
                ch = int(getattr(msg, "channel", -1))
                slot = per_channel.setdefault(ch, {"program": -1, "notes": 0})
                if slot["program"] < 0:
                    slot["program"] = int(msg.program)
            elif msg.type == "note_on" and int(getattr(msg, "velocity", 0)) > 0:
                ch = int(getattr(msg, "channel", -1))
                slot = per_channel.setdefault(ch, {"program": -1, "notes": 0})
                slot["notes"] += 1

        # Drop channels that never produced a note (tempo/meta-only entries
        # would just clutter the picker) AND drop drum channels — the
        # keys-import converter skips channel-9 events unconditionally,
        # so a drums entry would always yield an empty arrangement.
        active_channels = sorted(
            ch for ch, info in per_channel.items()
            if info["notes"] > 0 and ch != 9
        )

        if not active_channels:
            # Track with no melodic notes (silent or drums-only). Skip.
            continue

        # Format-0 with multiple non-drum channels is the only case where
        # we split. Type-1/2 keep one-entry-per-track so the user can
        # always import the whole part.
        split = split_format and len(active_channels) > 1

        if split:
            iter_channels = active_channels
        else:
            # One merged entry; channel comes from the first active one
            # for display purposes (and in case the converter ever needs
            # a hint, though channel_filter=None means "merge all").
            iter_channels = [active_channels[0]]

        for ch in iter_channels:
            info = per_channel[ch]
            program = info["program"]
            note_count = (
                info["notes"] if split
                else sum(per_channel[c]["notes"] for c in active_channels)
            )

            if split:
                channel_label = f"Ch{ch + 1}"
                base = name or f"Track {i}"
                entry_name = f"{base} — {channel_label}"
            else:
                entry_name = name or f"Track {i}"

            # Classify on the per-channel program first. The track-level
            # name hint is a tiebreaker only when no program_change was
            # seen for this channel — otherwise a track named "Piano"
            # that hosts bass on ch2 would wrongly flag ch2 as piano in
            # the format-0 split case. For non-split tracks the name
            # hint still carries weight (single-channel tracks usually
            # share track name + program intent).
            if program in _KEY_PROGRAMS:
                is_piano = True
            elif program < 0 and not split:
                # Program unknown for this single-channel track — fall
                # back to the track-name heuristic.
                name_lower = entry_name.lower()
                is_piano = any(hint in name_lower for hint in _KEYBOARD_NAME_HINTS)
            else:
                is_piano = False

            tracks.append({
                "index": i,
                # When set, the converter filters the track's events to this
                # channel only. None means "use every non-drum channel".
                "channel_filter": ch if split else None,
                "name": entry_name,
                "instrument": program,
                "notes": note_count,
                "channel": ch,
                "is_piano": bool(is_piano),
                # `is_drums` is always False on emitted entries because we
                # filter channel 9 above. Kept for shape compatibility
                # with the GP picker entries the frontend also reads.
                "is_drums": False,
                "strings": 0,
                "is_percussion": False,
            })

    return tracks


def convert_midi_track_to_keys_wire(
    midi_path: str,
    track_index: int,
    audio_offset: float = 0.0,
    name: str = "Keys",
    channel_filter: int | None = None,
) -> dict:
    """Convert a single MIDI track into a sloppak-format keys arrangement.

    Encodes each MIDI note as the piano plugin expects: string = pitch // 24,
    fret = pitch % 24 (so noteToMidi(s, f) = s * 24 + f recovers the pitch).
    Returns a wire-format arrangement dict ready to be written to
    arrangements/<id>.json.

    audio_offset (seconds) is added to every note's start time. Useful as a
    coarse pre-sync handle; finer alignment happens in the editor.

    channel_filter (optional): when set, only events on this channel are
    processed. Used by the picker to isolate one channel out of a format-0
    track that mixes multiple instruments.

    CC64 (sustain pedal) is honored: when a key is released while the pedal
    is held, the note's end time is extended to the pedal-up event on the
    same channel. Pedal-down/up transitions are tracked per channel.
    """
    midi = mido.MidiFile(midi_path)
    if track_index < 0 or track_index >= len(midi.tracks):
        raise ValueError(f"track_index {track_index} out of range")

    # Build a tempo map. The right scope depends on the SMF format:
    #   - type 0 (single track holding everything): the lone track is also
    #     the source of tempo events. Walking it (and only it) is correct.
    #   - type 1 (parallel tracks, shared timeline): tempo events live on
    #     the conductor track (usually track 0) but the spec allows them
    #     anywhere. Merge across all tracks so we don't miss any.
    #   - type 2 (independent sequential tracks, each its own timeline):
    #     a foreign track's tempo events do NOT apply to the chosen
    #     track. Merging would mis-time the notes — restrict the tempo
    #     scan to the selected track only.
    ticks_per_beat = midi.ticks_per_beat
    raw_events: list[tuple[int, int]] = [(0, 500000)]  # default 120 BPM
    midi_type = getattr(midi, "type", 1)
    tempo_source = (
        [midi.tracks[track_index]] if midi_type == 2 else midi.tracks
    )
    for track in tempo_source:
        abs_tick = 0
        for msg in track:
            abs_tick += msg.time
            if msg.type == "set_tempo":
                raw_events.append((abs_tick, int(msg.tempo)))
    raw_events.sort(key=lambda e: e[0])
    # Deduplicate at same tick (keep the last one written).
    deduped: list[tuple[int, int]] = []
    for ev in raw_events:
        if deduped and deduped[-1][0] == ev[0]:
            deduped[-1] = ev
        else:
            deduped.append(ev)

    # Precompute (tick, seconds_at_tick, microseconds_per_beat). seconds_at_tick
    # is the cumulative time up to that tempo-change event.
    tempo_table: list[tuple[int, float, int]] = []
    cum_seconds = 0.0
    prev_tick = 0
    prev_tempo = deduped[0][1]
    for ev_tick, ev_tempo in deduped:
        cum_seconds += (ev_tick - prev_tick) * (prev_tempo / 1_000_000.0) / ticks_per_beat
        tempo_table.append((ev_tick, cum_seconds, ev_tempo))
        prev_tick = ev_tick
        prev_tempo = ev_tempo
    tempo_ticks = [row[0] for row in tempo_table]

    def tick_to_seconds(tick: int) -> float:
        """O(log N) tempo-aware tick→seconds via cumulative table + bisect."""
        i = bisect_right(tempo_ticks, tick) - 1
        if i < 0:
            i = 0
        base_tick, base_seconds, tempo = tempo_table[i]
        return base_seconds + (tick - base_tick) * (tempo / 1_000_000.0) / ticks_per_beat

    # Walk the requested track, collect note_on/note_off pairs. To handle
    # rapid retriggers (same pitch starting again before the previous
    # note_off), keep a stack of start ticks per (channel, pitch).
    #
    # Sustain pedal (CC64): when value >= 64, the channel is "pedal down"
    # and key-release events don't truncate the note — they move the
    # pending start onto `pedal_pending`, where it waits for the pedal-up
    # transition. Pedal-up finalises every pending note on that channel
    # using the pedal-up tick as the end.
    track = midi.tracks[track_index]
    abs_tick = 0
    active: dict[tuple[int, int], deque[int]] = {}
    pedal_pending: dict[int, list[tuple[int, int]]] = {}  # ch -> [(pitch, start_tick)]
    pedal_down: dict[int, bool] = {}
    notes_out: list[dict] = []

    def _emit(pitch: int, start_tick: int, end_tick: int) -> None:
        t = tick_to_seconds(start_tick) + float(audio_offset)
        end = tick_to_seconds(end_tick) + float(audio_offset)
        notes_out.append({
            "t": round(t, 3),
            "s": int(pitch // 24),
            "f": int(pitch % 24),
            "sus": round(max(0.0, end - t), 3),
            "sl": -1, "slu": -1, "bn": 0,
            "ho": False, "po": False, "hm": False, "hp": False,
            "pm": False, "mt": False, "tr": False, "ac": False, "tp": False,
        })

    for msg in track:
        abs_tick += msg.time
        msg_ch = int(getattr(msg, "channel", -1))
        # Channel filter: when the picker entry was a format-0 split, only
        # process events on the chosen channel. Channel-less meta events
        # (set_tempo, etc.) have channel == -1 and pass through unaffected
        # because the message types we act on below all have a channel.
        if channel_filter is not None and msg_ch != -1 and msg_ch != channel_filter:
            continue

        if msg.type == "note_on" and int(getattr(msg, "velocity", 0)) > 0:
            if msg_ch == 9:
                continue  # skip percussion
            pitch = int(msg.note)
            active.setdefault((msg_ch, pitch), deque()).append(abs_tick)
        elif msg.type == "note_off" or (
            msg.type == "note_on" and int(getattr(msg, "velocity", 0)) == 0
        ):
            pitch = int(msg.note)
            stack = active.get((msg_ch, pitch))
            if not stack:
                continue
            # FIFO match against the oldest still-active start so overlapping
            # retriggers each get a sensible end time.
            start_tick = stack.popleft()
            if not stack:
                active.pop((msg_ch, pitch), None)
            if pedal_down.get(msg_ch, False):
                # Defer: extend the note until pedal-up.
                pedal_pending.setdefault(msg_ch, []).append((pitch, start_tick))
            else:
                _emit(pitch, start_tick, abs_tick)
        elif msg.type == "control_change" and int(getattr(msg, "control", -1)) == 64:
            was_down = pedal_down.get(msg_ch, False)
            now_down = int(getattr(msg, "value", 0)) >= 64
            pedal_down[msg_ch] = now_down
            if was_down and not now_down:
                # Pedal-up: finalise every pending note on this channel.
                pending = pedal_pending.pop(msg_ch, [])
                for pitch, start_tick in pending:
                    _emit(pitch, start_tick, abs_tick)

    # End-of-track: close anything still active or held by the pedal,
    # using abs_tick as the end. Pedaled notes that never saw a pedal-up
    # land here too.
    for (_ch, pitch), starts in active.items():
        for start_tick in starts:
            _emit(pitch, start_tick, abs_tick)
    active.clear()
    for _ch, pending in pedal_pending.items():
        for pitch, start_tick in pending:
            _emit(pitch, start_tick, abs_tick)
    pedal_pending.clear()

    notes_out.sort(key=lambda n: n["t"])

    return {
        "name": name,
        "tuning": [0, 0, 0, 0, 0, 0],
        "capo": 0,
        "notes": notes_out,
        "chords": [],
        "anchors": [],
        "handshapes": [],
        "templates": [],
    }
