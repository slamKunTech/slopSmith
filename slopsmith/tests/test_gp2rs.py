"""Tests for lib/gp2rs.py tempo/tick math helpers.

These functions are the pure arithmetic core of the Guitar Pro → Rocksmith
conversion pipeline. Fixture-free: all you need is hand-constructed
`TempoEvent` lists and integer tick / string inputs. The module-level
`import guitarpro` in gp2rs.py is harmless for import (no guitarpro
objects are constructed at module load).

See issue #46.
"""

import pytest

from gp2rs import (
    GP_TICKS_PER_QUARTER,
    TempoEvent,
    _gp_string_to_rs,
    _tempo_at_tick,
    _tick_to_seconds,
)


# ── _tick_to_seconds ─────────────────────────────────────────────────────────

def test_tick_to_seconds_at_zero():
    # Tick 0 is always time 0 regardless of tempo.
    tempo_map = [TempoEvent(tick=0, tempo=120.0)]
    assert _tick_to_seconds(0, tempo_map) == 0.0


def test_tick_to_seconds_constant_tempo():
    # At 120 BPM with 960 ticks/quarter, one quarter = 0.5s, so 1920 ticks = 1.0s.
    tempo_map = [TempoEvent(tick=0, tempo=120.0)]
    assert _tick_to_seconds(GP_TICKS_PER_QUARTER, tempo_map) == pytest.approx(0.5)
    assert _tick_to_seconds(2 * GP_TICKS_PER_QUARTER, tempo_map) == pytest.approx(1.0)
    assert _tick_to_seconds(4 * GP_TICKS_PER_QUARTER, tempo_map) == pytest.approx(2.0)


def test_tick_to_seconds_tempo_change_accumulates():
    # 4 quarter notes at 120 BPM = 2.0s, then 4 at 60 BPM = 4.0s. Total 6.0s.
    tempo_map = [
        TempoEvent(tick=0, tempo=120.0),
        TempoEvent(tick=4 * GP_TICKS_PER_QUARTER, tempo=60.0),
    ]
    # At the tempo-change boundary, time is 2.0 (4 beats at 120).
    assert _tick_to_seconds(4 * GP_TICKS_PER_QUARTER, tempo_map) == pytest.approx(2.0)
    # 4 more beats at 60 BPM = 4.0s. Total 6.0.
    assert _tick_to_seconds(8 * GP_TICKS_PER_QUARTER, tempo_map) == pytest.approx(6.0)


def test_tick_to_seconds_extrapolates_past_last_event():
    # Ticks past the last tempo event use that last event's tempo.
    tempo_map = [
        TempoEvent(tick=0, tempo=120.0),
        TempoEvent(tick=1000, tempo=240.0),
    ]
    # First 1000 ticks at 120 BPM = 1000/960 * 0.5 = 0.5208...s
    # Next 1000 ticks at 240 BPM = 1000/960 * 0.25 = 0.2604...s
    expected = (1000 / GP_TICKS_PER_QUARTER) * (60.0 / 120.0) + \
               (1000 / GP_TICKS_PER_QUARTER) * (60.0 / 240.0)
    assert _tick_to_seconds(2000, tempo_map) == pytest.approx(expected)


# ── _tempo_at_tick ───────────────────────────────────────────────────────────

def test_tempo_at_tick_before_first_event_returns_first_tempo():
    tempo_map = [TempoEvent(tick=100, tempo=120.0)]
    # Tick 0 is before the "first" event (which is at 100). Function starts
    # result at tempo_map[0].tempo and only updates when event.tick <= tick.
    assert _tempo_at_tick(0, tempo_map) == 120.0


def test_tempo_at_tick_at_exact_event():
    tempo_map = [
        TempoEvent(tick=0, tempo=120.0),
        TempoEvent(tick=500, tempo=200.0),
    ]
    assert _tempo_at_tick(500, tempo_map) == 200.0


def test_tempo_at_tick_between_events():
    tempo_map = [
        TempoEvent(tick=0, tempo=120.0),
        TempoEvent(tick=1000, tempo=200.0),
    ]
    assert _tempo_at_tick(500, tempo_map) == 120.0


def test_tempo_at_tick_past_last_event():
    tempo_map = [
        TempoEvent(tick=0, tempo=120.0),
        TempoEvent(tick=100, tempo=60.0),
        TempoEvent(tick=500, tempo=180.0),
    ]
    assert _tempo_at_tick(999999, tempo_map) == 180.0


def test_tempo_at_tick_single_event_map():
    tempo_map = [TempoEvent(tick=0, tempo=90.0)]
    assert _tempo_at_tick(0, tempo_map) == 90.0
    assert _tempo_at_tick(100000, tempo_map) == 90.0


# ── _gp_string_to_rs ─────────────────────────────────────────────────────────
# GP string numbering: 1 = highest pitch, N = lowest
# RS string numbering: 0 = lowest pitch (low E on a guitar)
# Transform: rs_index = num_strings - gp_string

@pytest.mark.parametrize("gp_string,num_strings,rs_index", [
    # 6-string guitar: GP 1 (high e) -> RS 5, GP 6 (low E) -> RS 0
    (1, 6, 5),
    (2, 6, 4),
    (3, 6, 3),
    (4, 6, 2),
    (5, 6, 1),
    (6, 6, 0),
    # 4-string bass: GP 1 (G) -> RS 3, GP 4 (E) -> RS 0
    (1, 4, 3),
    (2, 4, 2),
    (3, 4, 1),
    (4, 4, 0),
    # 7-string guitar: GP 1 (high e) -> RS 6, GP 7 (low B) -> RS 0
    (1, 7, 6),
    (7, 7, 0),
])
def test_gp_string_to_rs(gp_string, num_strings, rs_index):
    assert _gp_string_to_rs(gp_string, num_strings) == rs_index
