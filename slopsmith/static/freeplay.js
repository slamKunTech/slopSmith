// Free Play mode — a self-contained mini-highway that visualizes the
// guitar signal coming in through the Rocksmith cable in real time.
// Detected notes (string + fret + pitch) scroll up from the "now line"
// at the bottom and recede toward the horizon as they age.
//
// Why not reuse the highway.js singleton: its draw() loop is gated on
// `ready`, which is only flipped by the /ws/highway song WebSocket
// (closure-private, no public setter). Free Play has no song, so the
// loop would never draw. This module borrows highway's visual idiom
// (perspective trapezoid + per-string colours) but owns its canvas
// and rAF, leaving the player highway untouched.
//
// Input paths:
//   • Primary  — Web Audio getUserMedia + AnalyserNode FFT, full
//                polyphonic transcription via harmonic-sum multi-pitch
//                estimation across 6 strings × 25 frets.
//   • Fallback — JUCE getPitchDetection (monophonic) if getUserMedia
//                is unavailable. The note pipeline is identical either
//                way (single note in fallback = one entry in the map).
//
// Pitch → string/fret uses the same math as src/audio/ChordScorer.cpp:
// standard-tuning base MIDI per string [40,45,50,55,59,64]
// (E2 A2 D3 G3 B3 E4), fret = midi - open.

(function () {
    'use strict';

    // ── Tuning tables (aligned with lib/tunings.py; offsets[0] = low E) ─
    const BASE_STD = [40, 45, 50, 55, 59, 64]; // E2 A2 D3 G3 B3 E4
    const TUNINGS = {
        'std-E':   [0, 0, 0, 0, 0, 0],
        'std-Eb':  [-1, -1, -1, -1, -1, -1],
        'std-D':   [-2, -2, -2, -2, -2, -2],
        'std-Cs':  [-3, -3, -3, -3, -3, -3],
        'std-C':   [-4, -4, -4, -4, -4, -4],
        'std-B':   [-5, -5, -5, -5, -5, -5],
        'std-Bb':  [-6, -6, -6, -6, -6, -6],
        'std-A':   [-7, -7, -7, -7, -7, -7],
        'std-F':   [1, 1, 1, 1, 1, 1],
        'std-Fs':  [2, 2, 2, 2, 2, 2],
        'drop-D':  [-2, 0, 0, 0, 0, 0],
        'drop-Cs': [-3, -1, -1, -1, -1, -1],
        'drop-C':  [-4, -2, -2, -2, -2, -2],
        'drop-B':  [-5, -3, -3, -3, -3, -3],
        'drop-Bb': [-6, -4, -4, -4, -4, -4],
        'drop-A':  [-7, -5, -5, -5, -5, -5],
        'ddrop-D': [-2, -2, 0, 0, 0, 0],
        'open-G':  [0, 0, 0, -1, 0, 0],
        'open-D':  [-2, -2, 0, 0, -2, -2],
        'dadgad':  [-2, 0, 0, 0, -2, 0],
        'open-E':  [0, 2, 2, 1, 0, 0],
    };

    const STRING_COLORS = ['#3b82f6', '#10b981', '#eab308', '#f97316', '#ef4444', '#a855f7'];
    const STRING_NAMES = ['E', 'A', 'D', 'G', 'B', 'e'];
    const NOTE_NAMES = ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B'];

    const STRING_COUNT = 6;
    const MAX_FRET = 24;
    const VISIBLE_SECONDS = 3.0;
    const CULL_SECONDS = VISIBLE_SECONDS + 0.5;
    const CONFIDENCE_GATE = 0.5;        // fallback monophonic confidence floor
    const POLL_INTERVAL_MS = 33;
    const MIN_NOTE_SUS = 0.08;
    const MISS_GRACE_FRAMES = 3;        // frames (×~33ms) a note may vanish before closing — kills flicker/shattered blocks
    const ONSET_FRAMES = 2;             // consecutive detections before a note starts — rejects attack-transient ghosts
    const SILENCE_OVERLAY_MS = 2000;

    // Polyphonic detection params
    const FFT_SIZE = 8192;
    const HARMONICS = 5;
    const FUND_WEIGHT = 1.0;
    const HARM_WEIGHT = 0.5;
    const ABS_FLOOR_DB = -85;           // below this a bin is treated as silence
    const REL_GATE = 0.12;              // a fundamental must be ≥12% of this frame's peak (low E needs headroom)
    const SNR_MIN_DB = 15;              // candidate bin must clear the adaptive noise floor by this much
    const MIN_PEAK_DB = -70;            // frame peak must clear this for ANY detection — kills ghost notes on a silent/muted input

    // ── Pick-attack (onset) detection ──────────────────────────────────────
    // Spectral flux = sum of positive per-bin dB deltas vs the previous frame.
    // A fresh pick dumps broadband energy in at once → a flux spike; a decaying
    // sustain only goes down → near-zero flux. This is the signal that lets
    // the practice pipeline tell "a new pick" apart from "the same note still
    // ringing", which is what 不同拨弦识别 hinges on.
    //
    // Live effect: a strong attack (_frameRepick) on a string:fret that is
    // already ringing splits it into a second note, so repeated picks of the
    // same note are scored as separate notes instead of one long sustain.
    // The flags are also exposed on window.freeplay.__debug for diagnostics.
    //
    // We deliberately do NOT bypass the ONSET_FRAMES debounce on onset: the
    // first frame of a pick is attack-transient-dominated and confirming a
    // candidate immediately there risks locking a wrong fret. Learn mode's
    // EARLY_GRACE / WAIT_LOOKBACK already absorb the debounce latency.
    const ONSET_FLUX_ABS = 60;          // flux floor — idle noise/hum never reads as an attack
    const ONSET_FLUX_MULT = 1.8;        // flux must beat this × its own EMA to count as an onset
    const REPICK_FLUX_MULT = 2.6;       // stricter gate for splitting a ringing note (avoids fragmenting clean sustains)

    // ── Debug logging ────────────────────────────────────────────────────
    // The detection loop runs ~30×/s. Emitting formatted console.log lines
    // every frame (spectrum dumps, raw/per-note detection, suppression
    // reasons) is a real hot-path cost — string interpolation + console I/O
    // on the audio thread's UI counterpart — and it floods DevTools so the
    // useful lines scroll away. Gate ALL of it behind DEBUG, off by default.
    // Enable for troubleshooting without a code edit via:
    //   • localStorage.setItem('fpDebug','1')  (persist across reloads), or
    //   • the ?fpdebug=1 query string          (one-off).
    const DEBUG = (function () {
        try {
            if (localStorage.getItem('fpDebug') === '1') return true;
            if (/[?&]fpdebug=1\b/.test(location.search)) return true;
        } catch { /* storage blocked / non-browser */ }
        return false;
    })();
    const dbg = DEBUG ? (...a) => console.log(...a) : () => {};

    // ── State ────────────────────────────────────────────────────────────
    let canvas = null, ctx = null, rafId = null, running = false;
    let lastPollAt = 0, pollInFlight = false;
    let lastHeardAt = 0;
    let audioAvailable = false;        // JUCE engine present (fallback path)
    let mirrored = false;              // mirror strings: 123456 ↔ 654321
    let tilted = false;                // tilt highway 45 degrees right (clockwise)

    // Web Audio graph
    let audioCtx = null, analyser = null, freqData = null, micStream = null;
    let webAudioOk = false, sampleRate = 48000;
    let lastDiagAt = 0;
    let lastPeakDb = -Infinity;
    // Pick-attack detector state (see ONSET_FLUX_* constants).
    let _prevDb = null;        // previous frame's dB spectrum (Float32Array)
    let _fluxEma = 0;          // exponential moving average of spectral flux
    let _frameFlux = 0;        // this frame's raw flux
    let _frameOnset = false;   // this frame carries a pick attack
    let _frameRepick = false;  // strong attack — safe to split a ringing note

    // Note pipeline (shared by polyphonic + fallback)
    let notes = [];                    // closed notes: {t, s, f, sus}  (t = abs seconds)
    let activeNotes = new Map();       // key "s:f" -> {t, s, f, midi}  (currently ringing)
    let pending = new Map();           // key "s:f" -> {s, f, midi, seen} (onset debounce)

    // Recording / playback
    let recording = false, recStart = 0;
    let session = [];                  // [{t, s, f, sus}]  (t = seconds from recStart)
    let sessionDur = 0;
    let playback = null;               // {start, loop} or null
    let loopOn = false;

    // Recording export
    let mediaRecorder = null, recChunks = [], audioBlob = null, recMime = '';

    // Learn-mode song reference: pins the detection grid to the song's
    // tuning/capo (non-standard tunings are otherwise undetectable).
    let _refOffsets = null, _refCapo = null;

    // HUD element refs
    let elNote, elFreq, elPos, elOverlay, elOverlayTitle, elOverlayBody;
    let elTuning, elCapo, elDevice, elRec, elPlay, elLoopBtn, elClear, elRecTime;
    let elExpMidi, elExpAudio;
    let elRootNote, elRootFreq, elMirrorBtn, elTiltBtn;

    function $(id) { return document.getElementById(id); }

    // ── Tuning / capo helpers ────────────────────────────────────────────
    // Learn mode overrides these via setSongReference() so detection uses
    // the song's actual tuning/capo instead of the Free Play UI selectors.
    function currentOffsets() {
        if (_refOffsets) return _refOffsets;
        return TUNINGS[elTuning ? elTuning.value : 'std-E'] || TUNINGS['std-E'];
    }
    function currentCapo() {
        if (_refCapo !== null) return _refCapo;
        const v = elCapo ? parseInt(elCapo.value, 10) : 0;
        return Number.isFinite(v) ? Math.max(0, Math.min(12, v)) : 0;
    }
    function openMidi(s) { return BASE_STD[s] + currentOffsets()[s] + currentCapo(); }

    function midiToName(midi) {
        if (midi < 0) return '—';
        return NOTE_NAMES[midi % 12] + (Math.floor(midi / 12) - 1);
    }

    // Monophonic fallback: nearest string/fret for a single MIDI note.
    function resolveStringFret(midi) {
        let best = null;
        for (let s = 0; s < BASE_STD.length; s++) {
            const fret = midi - openMidi(s);
            if (fret < 0 || fret > MAX_FRET) continue;
            if (!best || fret < best.fret) best = { s, f: fret };
        }
        return best;
    }

    // ── Canvas sizing ────────────────────────────────────────────────────
    function resize() {
        if (!canvas) return;
        const w = canvas.clientWidth || (canvas.parentElement && canvas.parentElement.clientWidth) || 800;
        const h = canvas.clientHeight || (canvas.parentElement && canvas.parentElement.clientHeight) || 600;
        canvas.width = Math.round(w);
        canvas.height = Math.round(h);
    }

    // ── Projection (p: 0=now/bottom → 1=horizon/top) ─────────────────────
    function project(p) {
        const W = canvas.width, H = canvas.height;
        const nearY = H - 8;
        const farY = H * 0.10;
        const nearHalf = W * 0.42;
        const farHalf = W * 0.06;
        const pp = Math.max(0, Math.min(1, p));
        const y = nearY - (nearY - farY) * pp;
        const halfW = nearHalf + (farHalf - nearHalf) * pp;
        const scale = 1 - 0.85 * pp;
        return { y, halfW, scale, nearY, farY, nearHalf, farHalf, W, H };
    }
    function laneX(s, pr) {
        // Apply mirroring: if mirrored, reverse string order
        const effectiveS = mirrored ? (STRING_COUNT - 1 - s) : s;
        const off = (effectiveS - (STRING_COUNT - 1) / 2) / ((STRING_COUNT - 1) / 2);
        return pr.W / 2 + off * pr.halfW;
    }

    // ── Drawing ──────────────────────────────────────────────────────────
    function drawBackground() {
        const W = canvas.width, H = canvas.height;
        const top = project(1), bot = project(0);
        const cx = W / 2;

        // Apply tilt transform if enabled
        if (tilted) {
            ctx.save();
            ctx.translate(W / 2, H / 2);
            ctx.rotate(Math.PI / 4); // 45 degrees clockwise (right)
            ctx.translate(-W / 2, -H / 2);
        }

        ctx.fillStyle = '#0a0a14';
        ctx.beginPath();
        ctx.moveTo(cx - top.halfW, top.y); ctx.lineTo(cx + top.halfW, top.y);
        ctx.lineTo(cx + bot.halfW, bot.y); ctx.lineTo(cx - bot.halfW, bot.y);
        ctx.closePath(); ctx.fill();
        ctx.strokeStyle = 'rgba(120,120,180,0.25)'; ctx.lineWidth = 1; ctx.stroke();
        for (let s = 0; s < STRING_COUNT; s++) {
            ctx.strokeStyle = STRING_COLORS[s] + '55'; ctx.lineWidth = 1.5;
            ctx.beginPath();
            ctx.moveTo(laneX(s, top), top.y); ctx.lineTo(laneX(s, bot), bot.y);
            ctx.stroke();
        }
        ctx.strokeStyle = 'rgba(255,255,255,0.35)'; ctx.lineWidth = 2;
        ctx.beginPath();
        ctx.moveTo(cx - bot.halfW, bot.nearY); ctx.lineTo(cx + bot.halfW, bot.nearY);
        ctx.stroke();

        if (tilted) {
            ctx.restore();
        }
    }

    function drawNote(note, clock, alpha) {
        // note occupies [t, t+sus]; newest end (bottom) at age (clock-(t+sus)),
        // oldest end (top) at age (clock-t).
        const ageBot = (clock - (note.t + note.sus)) / VISIBLE_SECONDS;
        const ageTop = (clock - note.t) / VISIBLE_SECONDS;
        if (ageTop >= 1) return;
        const pBot = project(ageBot);
        const pTop = project(ageTop);
        const xBot = laneX(note.s, pBot);
        const xTop = laneX(note.s, pTop);
        const wBot = 18 * pBot.scale, wTop = 18 * pTop.scale;

        if (tilted) {
            ctx.save();
            ctx.translate(canvas.width / 2, canvas.height / 2);
            ctx.rotate(Math.PI / 4); // 45 degrees clockwise (right)
            ctx.translate(-canvas.width / 2, -canvas.height / 2);
        }

        ctx.globalAlpha = alpha == null ? 1 : alpha;
        ctx.fillStyle = STRING_COLORS[note.s];
        ctx.strokeStyle = 'rgba(0,0,0,0.4)'; ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.moveTo(xBot - wBot / 2, pBot.y); ctx.lineTo(xBot + wBot / 2, pBot.y);
        ctx.lineTo(xTop + wTop / 2, pTop.y); ctx.lineTo(xTop - wTop / 2, pTop.y);
        ctx.closePath(); ctx.fill(); ctx.stroke();
        ctx.globalAlpha = 1;

        if (ageBot < 1) {
            ctx.fillStyle = '#fff';
            ctx.font = `${Math.max(9, Math.round(11 * pBot.scale))}px ui-sans-serif, system-ui`;
            ctx.textAlign = 'center'; ctx.textBaseline = 'middle';
            ctx.fillText(String(note.f), xBot, pBot.y);
        }

        if (tilted) {
            ctx.restore();
        }
    }

    function draw(now) {
        if (!ctx) return;
        ctx.clearRect(0, 0, canvas.width, canvas.height);
        drawBackground();
        for (const n of notes) drawNote(n, now, 1);
        for (const entry of activeNotes.values()) {
            drawNote({ t: entry.t, s: entry.s, f: entry.f,
                       sus: Math.max(MIN_NOTE_SUS, now - entry.t) }, now, 1);
        }
        if (playback) {
            let pbNow = now - playback.start;
            if (pbNow > sessionDur) {
                if (loopOn && sessionDur > 0) { playback.start += sessionDur; pbNow = now - playback.start; }
                else { playback = null; updateRecButtons(); }
            }
            if (playback) for (const n of session) drawNote(n, pbNow, 0.55);
        }
    }

    // ── Polyphonic detection (Web Audio FFT → harmonic-sum) ──────────────
    function detectPolyphonic() {
        if (!analyser || !freqData) return [];
        analyser.getFloatFrequencyData(freqData);
        const binHz = sampleRate / FFT_SIZE;
        // Adaptive noise gate: the frame's median bin level estimates the
        // broadband noise floor (hiss, hum, fan noise). A real plucked
        // note's fundamental sticks far above it; idle noise is roughly
        // uniform, so nothing clears the gate — no ghost notes while the
        // guitar is silent. Median is robust against the few strong bins
        // a real note excites, so the estimate stays valid while playing.
        //
        // Perf: computing the median by sorting all 4096 bins every frame
        // (~30 fps) is an O(n log n) hot-path cost. getFloatFrequencyData
        // returns dB in roughly [-120, 0], so we bucket into a fixed 0.5 dB
        // histogram and walk it to the 50th percentile — single O(n) pass,
        // and 0.5 dB quantisation is far finer than the 15 dB SNR gate needs.
        const HIST_LO = -120, HIST_STEP = 0.5, HIST_N = 240; // covers [-120, 0]
        const hist = new Int32Array(HIST_N + 1);
        let finiteCount = 0;
        let framePeakDb = -Infinity;
        // Spectral flux (pick attack) folded into this same single pass: sum
        // of the positive per-bin dB deltas vs the previous frame. No extra
        // O(n) sweep.
        if (!_prevDb || _prevDb.length !== freqData.length) {
            _prevDb = new Float32Array(freqData.length);
            _prevDb.fill(-Infinity);
        }
        let flux = 0;
        for (let i = 0; i < freqData.length; i++) {
            const db = freqData[i];
            if (!isFinite(db)) continue;
            finiteCount++;
            if (db > framePeakDb) framePeakDb = db;
            const d = db - _prevDb[i];
            if (d > 0) flux += d;
            _prevDb[i] = db;
            let b = Math.round((db - HIST_LO) / HIST_STEP);
            if (b < 0) b = 0; else if (b > HIST_N) b = HIST_N;
            hist[b]++;
        }
        lastPeakDb = framePeakDb;
        // Onset gate uses the EMA from BEFORE this frame is folded in, so a
        // sudden spike still towers over the running average.
        _frameFlux = flux;
        const onsetGate = Math.max(ONSET_FLUX_ABS, ONSET_FLUX_MULT * _fluxEma);
        const repickGate = Math.max(ONSET_FLUX_ABS, REPICK_FLUX_MULT * _fluxEma);
        _fluxEma = _fluxEma * 0.9 + flux * 0.1;
        let noiseDb;
        if (finiteCount >= 16) {
            const half = finiteCount >> 1;
            let acc = 0, mb = 0;
            for (; mb <= HIST_N; mb++) { acc += hist[mb]; if (acc > half) break; }
            noiseDb = HIST_LO + mb * HIST_STEP;
        } else {
            noiseDb = ABS_FLOOR_DB; // near-total silence: fall back to absolute floor
        }
        // Throttled spectrum diagnostic for the low string: one line per
        // second with the dB levels of the low E fundamental + first
        // harmonics. Pluck 6弦 once and paste the last few lines.
        // Gated behind DEBUG so the hot path stays silent in normal use.
        if (DEBUG) {
            const t = performance.now();
            if (t - lastDiagAt > 1000) {
                lastDiagAt = t;
                const f0 = 440 * Math.pow(2, (openMidi(0) - 69) / 12);
                const dbAt = (hz) => {
                    const b = Math.round(hz / binHz);
                    return (b >= 0 && b < freqData.length && isFinite(freqData[b]))
                        ? freqData[b].toFixed(1) + 'dB' : 'sil';
                };
                const devLabel = micStream && micStream.getAudioTracks().length
                    ? micStream.getAudioTracks()[0].label : 'none';
                dbg(`=== 6弦频谱 === dev=${devLabel} peak=${framePeakDb.toFixed(1)}dB noise=${noiseDb.toFixed(1)}dB gate=${(noiseDb + SNR_MIN_DB).toFixed(1)}dB`
                    + ` | E2=${dbAt(f0)} 2nd=${dbAt(f0 * 2)} 3rd=${dbAt(f0 * 3)} 4th=${dbAt(f0 * 4)} 5th=${dbAt(f0 * 5)}`);
            }
        }
        // A real pluck peaks tens of dB above converter hiss. If the
        // whole frame sits below MIN_PEAK_DB the input is silent
        // (wrong/muted device, unplugged cable) — bail out instead of
        // letting the degenerate relative gate turn numeric noise into
        // ghost notes on random strings.
        if (framePeakDb < MIN_PEAK_DB) { _frameOnset = false; _frameRepick = false; return []; }
        _frameOnset = flux > onsetGate;
        _frameRepick = flux > repickGate;
        const linAt = (bin) => {
            if (bin < 0 || bin >= freqData.length) return 0;
            const db = freqData[bin];
            if (!isFinite(db) || db <= ABS_FLOOR_DB) return 0;
            if (db <= noiseDb + SNR_MIN_DB) return 0;
            return Math.pow(10, db / 20);
        };

        // Score every (string, fret) candidate by harmonic sum, track
        // the frame's peak fundamental energy for the relative gate.
        let peakFund = 0;
        const scored = []; // {s, f, midi, score, fund}
        for (let s = 0; s < STRING_COUNT; s++) {
            const open = openMidi(s);
            for (let f = 0; f <= MAX_FRET; f++) {
                const midi = open + f;
                const f0 = 440 * Math.pow(2, (midi - 69) / 12);
                const fundBin = Math.round(f0 / binHz);
                const fund = linAt(fundBin);
                if (fund <= 0) continue;
                // Harmonic score: weight fundamental heavily, harmonics lightly.
                // Keep per-harmonic bin levels — the ghost suppressor later
                // predicts a ghost's expected energy from the note's own
                // harmonic series instead of guessing from its score.
                let score = FUND_WEIGHT * fund;
                const harms = [fund];
                for (let h = 2; h <= HARMONICS; h++) {
                    const amp = linAt(Math.round((f0 * h) / binHz));
                    harms.push(amp);
                    score += HARM_WEIGHT * amp;
                }
                // Fundamental-presence gate: a real plucked string's
                // fundamental decays faster than its harmonics, so
                // demanding dominance (0.5) rejects nearly every real
                // note — 1弦 notes die mid-sustain and the weak low-E
                // fundamental is skipped outright. The noise gate already
                // rejects candidates with NO fundamental energy, so this
                // only needs to catch near-absent leakage — 0.05.
                // (A weak fundamental still carries real pitch info and
                // the harmonic sum does the heavy lifting.)
                const fundRatio = (FUND_WEIGHT * fund) / score;
                if (fundRatio < 0.05) continue;
                scored.push({ s, f, midi, score, fund, harms });
                if (fund > peakFund) peakFund = fund;
            }
        }
        if (peakFund <= 0) return [];
        // Relative gate: a candidate's fundamental must be at least REL_GATE
        // of this frame's loudest fundamental to count. ABS_FLOOR_DB (in
        // linAt) already zeroes sub-noise bins, so this only rejects quiet
        // ghosts sitting beside a much louder real note.
        const gate = REL_GATE * peakFund;

        // Per string: pick the strongest fret above gate that is a local
        // maximum (louder than its neighbours on the same string).
        const perString = new Array(STRING_COUNT).fill(null);
        scored.sort((a, b) => a.f - b.f);
        const byString = {};
        for (const c of scored) { (byString[c.s] = byString[c.s] || []).push(c); }
        for (let s = 0; s < STRING_COUNT; s++) {
            const arr = byString[s] || [];
            if (!arr.length) continue;
            let bestScore = 0;
            for (const c of arr) {
                if (c.fund < gate) continue;
                if (c.score > bestScore) bestScore = c.score;
            }
            if (bestScore <= 0) continue;
            // Walk ascending fret. The first gated candidate within a
            // factor of 4 of the string's best score wins — that's the
            // played note. Later candidates whose fundamental sits at an
            // integer multiple of it are its harmonics (low E's octave at
            // fret 12 is often LOUDER than the fundamental) and are
            // skipped so they can't steal the string. The window is
            // generous because preamped guitars (e.g. Enya Nova Go) roll
            // off the low-string fundamental hard.
            let pick = null;
            for (const c of arr) {
                if (c.fund < gate) continue;
                if (pick) {
                    const r = Math.pow(2, (c.midi - pick.midi) / 12);
                    const n = Math.round(r);
                    if (r > 1.05 && r < 16.1 && Math.abs(r - n) < 0.03) continue; // harmonic of pick
                }
                if (c.score >= bestScore * 0.25) { pick = c; break; }
            }
            perString[s] = pick;
        }

        // Cross-string dedup + harmonic-ghost suppression.
        let winners = perString.filter(Boolean);

        // If only one winner detected, return it immediately (common case: single string plucked)
        if (winners.length === 1) {
            return [{ s: winners[0].s, f: winners[0].f, midi: winners[0].midi }];
        }

        // DEBUG: Log what each string detected before filtering (gated).
        if (DEBUG && winners.length > 0) {
            dbg(`=== Raw detection === (noiseFloor=${noiseDb.toFixed(1)}dB)`);
            winners.forEach(w => {
                const note = ['C','C#','D','D#','E','F','F#','G','G#','A','A#','B'][w.midi % 12];
                const octave = Math.floor(w.midi / 12) - 1;
                dbg(`String ${w.s+1}: fret ${w.f}, ${note}${octave} (MIDI ${w.midi}), fund=${w.fund.toFixed(2)}, score=${w.score.toFixed(2)}`);
            });
        }

        // Chord-vs-single-note arbitration for resonant low strings.
        //
        // The per-string stage above already yields at most ONE candidate per
        // string (which is exactly the physical model of a guitar chord), each
        // having cleared REL_GATE (fundamental ≥ 12% of the frame peak) with
        // same-string harmonics skipped. So `winners` is already a credible
        // chord. What remains is to drop (a) sympathetic resonance from
        // neighbouring strings and (b) octave/harmonic ghosts — WITHOUT
        // collapsing genuine chords down to a single note.

        // Find the absolute strongest candidate by total score
        let strongest = winners[0];
        for (const w of winners) {
            if (w.score > strongest.score) {
                strongest = w;
            }
        }

        // PHYSICAL STRING PREFERENCE: when resonance is strong, prefer the string
        // that is physically expected to produce this pitch (lower string for lower pitch)
        // E.g., when 5弦A2 and 3弦G3 both detected, 5弦 is lower pitch on lower string = expected
        winners.sort((a, b) => a.midi - b.midi); // sort by pitch ascending

        // For each detected pitch, prefer the string with the LOWEST string number (thickest)
        // that can physically produce it (open string or fretted position exists)
        for (let i = 0; i < winners.length; i++) {
            const current = winners[i];
            // Check if a lower (thicker) string also detected a similar or lower pitch
            for (let j = 0; j < i; j++) {
                const lower = winners[j];
                if (lower.s > current.s) { // lower string number = thicker string
                    // If thicker string has similar or lower pitch, it's the real source
                    // The thinner string's detection is sympathetic resonance
                    if (Math.abs(lower.midi - current.midi) <= 7) { // within a 5th
                        current.likelyResonance = true;
                        dbg(`String ${current.s+1} likely resonance from string ${lower.s+1}`);
                    }
                }
            }
        }

        // Filter out likely resonances before applying score threshold
        winners = winners.filter(w => !w.likelyResonance);

        // Re-find strongest after resonance filtering
        strongest = winners[0];
        for (const w of winners) {
            if (w.score > strongest.score) {
                strongest = w;
            }
        }

        // Keep other candidates that clear CHORD_THRESHOLD of the strongest
        // score and aren't an octave/harmonic of it.
        //
        // 0.45 (was 0.80): the previous 0.80 gate forced every chord member
        // within 20% of the loudest string, which rejected most real strummed
        // chords — bass strings ring noticeably louder than treble, so genuine
        // chord members routinely span 2-3× in energy. That silently turned
        // 和弦练习 into single-note detection. REL_GATE + the resonance filter
        // above already remove weak ghosts, so a far more permissive energy
        // gate is both safe and correct here.
        const CHORD_THRESHOLD = 0.45;
        const kept = [strongest];

        for (const c of winners) {
            if (c === strongest) continue;

            // Chord test: similar energy level
            if (c.score >= strongest.score * CHORD_THRESHOLD) {
                // Additional check: not a harmonic relationship
                const cFreq = 440 * Math.pow(2, (c.midi - 69) / 12);
                const sFreq = 440 * Math.pow(2, (strongest.midi - 69) / 12);
                const ratio = cFreq / sFreq;
                const invRatio = sFreq / cFreq;

                let isHarmonic = false;
                // Check if c is a harmonic of strongest
                if (ratio > 1.9 && ratio < 16.1) {
                    const n = Math.round(ratio);
                    if (Math.abs(ratio - n) < 0.03) {
                        isHarmonic = true;
                        dbg(`Suppressed: String ${c.s+1} is ${n}th harmonic of strongest string ${strongest.s+1}`);
                    }
                }
                // Check if strongest is a harmonic of c
                if (!isHarmonic && invRatio > 1.9 && invRatio < 16.1) {
                    const n = Math.round(invRatio);
                    if (Math.abs(invRatio - n) < 0.03) {
                        isHarmonic = true;
                        dbg(`Suppressed: String ${c.s+1} (strongest is its ${n}th harmonic)`);
                    }
                }

                if (!isHarmonic) {
                    kept.push(c);
                }
            } else {
                dbg(`Suppressed: String ${c.s+1} too weak (${(c.score/strongest.score*100).toFixed(0)}% of strongest)`);
            }
        }

        // DEBUG: Log what survived filtering (gated).
        if (DEBUG && kept.length > 0 && kept.length !== winners.length) {
            dbg('=== After filtering ===');
            kept.forEach(k => {
                const note = ['C','C#','D','D#','E','F','F#','G','G#','A','A#','B'][k.midi % 12];
                const octave = Math.floor(k.midi / 12) - 1;
                dbg(`Kept: String ${k.s+1}, ${note}${octave} (MIDI ${k.midi}), score=${k.score.toFixed(2)}`);
            });
        }

        // Same MIDI pitch on two strings → keep the stronger (lower fret on tie).
        const byMidi = {};
        for (const c of kept) (byMidi[c.midi] = byMidi[c.midi] || []).push(c);
        const out = [];
        for (const midi in byMidi) {
            const opts = byMidi[midi];
            opts.sort((a, b) => b.score - a.score || a.f - b.f);
            out.push({ s: opts[0].s, f: opts[0].f, midi: opts[0].midi });
        }
        return out;
    }

    // ── Detection dispatch ───────────────────────────────────────────────
    async function detect(now) {
        if (webAudioOk && analyser) return detectPolyphonic();
        const audio = window.slopsmithDesktop && window.slopsmithDesktop.audio;
        if (!audio) return [];
        let pitch = null;
        try { pitch = await audio.getPitchDetection(); } catch { pitch = null; }
        if (pitch && pitch.midiNote >= 0 && pitch.confidence >= CONFIDENCE_GATE) {
            const r = resolveStringFret(pitch.midiNote);
            if (r) return [{ s: r.s, f: r.f, midi: pitch.midiNote, pitch }];
        }
        return [];
    }

    // ── Note pipeline (apply this frame's detected set) ──────────────────
    function closeNote(entry, now) {
        const sus = Math.max(MIN_NOTE_SUS, now - entry.t);
        notes.push({ t: entry.t, s: entry.s, f: entry.f, sus });
        if (recording) {
            const relT = Math.max(0, entry.t - recStart);
            // Store midi so MIDI export is correct regardless of the
            // tuning selected at export time (the pitch was fixed when
            // the note was played).
            session.push({ t: relT, s: entry.s, f: entry.f, sus, midi: entry.midi });
            if (relT + sus > sessionDur) sessionDur = relT + sus;
        }
    }

    function applyDetected(detected, now) {
        const keys = new Set(detected.map(d => d.s + ':' + d.f));
        for (const d of detected) {
            const k = d.s + ':' + d.f;
            let entry = activeNotes.get(k);
            if (entry) {
                entry.misses = 0;
                // Re-pick split: an unambiguous fresh attack on a string:fret
                // that is ALREADY ringing (and has sounded long enough to be a
                // real note, not a just-started one) means the player picked it
                // again. Close the old note and open a new one so repeated
                // picks register as separate notes instead of merging into one
                // long sustain — critical for rhythm/chord practice scoring.
                if (_frameRepick && (now - entry.t) >= MIN_NOTE_SUS) {
                    closeNote(entry, now);
                    activeNotes.set(k, { t: now, s: d.s, f: d.f, midi: d.midi, misses: 0 });
                }
            } else if (pending.has(k)) {
                const p = pending.get(k);
                p.seen++;
                if (p.seen >= ONSET_FRAMES) {
                    pending.delete(k);
                    activeNotes.set(k, { t: now, s: d.s, f: d.f, midi: d.midi, misses: 0 });
                }
            } else {
                pending.set(k, { s: d.s, f: d.f, midi: d.midi, seen: 1 });
            }
        }
        // Pending candidates that vanished are transient noise — drop.
        for (const [k] of pending) if (!keys.has(k)) pending.delete(k);
        for (const [k, entry] of activeNotes) {
            if (!keys.has(k)) {
                // Grace period: a note may flicker out of detection for a
                // few frames mid-sustain (fundamental dips below the gate
                // while the string still rings). Closing instantly is what
                // turns a sustain into short, chopped blocks.
                entry.misses++;
                if (entry.misses > MISS_GRACE_FRAMES) { closeNote(entry, now); activeNotes.delete(k); }
            }
        }
    }

    // ── HUD + overlay ────────────────────────────────────────────────────
    function updateHud(detected) {
        if (!elNote) return;
        if (detected.length) {
            elNote.textContent = detected.map(d => midiToName(d.midi)).join(' ');
            elFreq.textContent = '';
            elPos.textContent = detected.map(d => STRING_NAMES[d.s] + d.f).join(' · ');

            // Update root note display (lowest detected MIDI note)
            const rootMidi = Math.min(...detected.map(d => d.midi));
            const rootFreq = 440 * Math.pow(2, (rootMidi - 69) / 12);
            if (elRootNote) {
                elRootNote.textContent = midiToName(rootMidi);
            }
            if (elRootFreq) {
                elRootFreq.textContent = rootFreq.toFixed(1) + ' Hz';
            }
        } else {
            elNote.textContent = '—'; elFreq.textContent = ''; elPos.textContent = '';
            if (elRootNote) elRootNote.textContent = '—';
            if (elRootFreq) elRootFreq.textContent = '— Hz';
        }
    }

    function updateOverlay(now) {
        if (!elOverlay) return;
        let show = false, title = 'Waiting for guitar input…';
        let body = 'Plug in your Rocksmith cable and pick it in the Input dropdown. Free Play captures it directly via Web Audio.';
        if (!webAudioOk && !audioAvailable) {
            show = true; title = 'No audio input available';
            body = 'Web Audio capture failed and the native audio engine is unavailable. Plug in your Rocksmith cable and retry.';
        } else if (webAudioOk && lastPeakDb < -90) {
            show = true; title = 'Selected input is silent';
            body = `The selected device is delivering no signal (peak ${lastPeakDb.toFixed(0)}dB). Pick your guitar input in the dropdown — e.g. the C-Media / Rocksmith adapter.`;
        } else if (now - lastHeardAt > SILENCE_OVERLAY_MS / 1000) {
            show = true;
        }
        elOverlay.classList.toggle('hidden', !show);
        if (show) { elOverlayTitle.textContent = title; elOverlayBody.textContent = body; }
    }

    function fmtTime(s) {
        const m = Math.floor(s / 60), ss = Math.floor(s % 60);
        return m + ':' + String(ss).padStart(2, '0');
    }

    function updateRecButtons() {
        if (!elRec) return;
        elRec.textContent = recording ? '■ Stop' : '● Rec';
        elRec.classList.toggle('bg-red-900/60', recording);
        elPlay.disabled = session.length === 0 || recording;
        elPlay.textContent = playback ? '⏸ Stop' : '▶ Play';
        elLoopBtn.classList.toggle('text-accent', loopOn);
        elLoopBtn.classList.toggle('text-gray-500', !loopOn);
        if (elExpMidi) elExpMidi.disabled = session.length === 0 || recording;
        if (elExpAudio) elExpAudio.disabled = !audioBlob || recording;
        if (elRecTime) {
            if (recording) elRecTime.textContent = fmtTime((performance.now() / 1000) - recStart);
            else if (playback) elRecTime.textContent = fmtTime((performance.now() / 1000) - playback.start) + ' / ' + fmtTime(sessionDur);
            else if (sessionDur > 0) elRecTime.textContent = fmtTime(sessionDur);
            else elRecTime.textContent = '';
        }
    }

    // ── Web Audio setup ──────────────────────────────────────────────────
    async function acquireStream(deviceId) {
        const constraints = {
            audio: {
                echoCancellation: false, noiseSuppression: false, autoGainControl: false,
            },
            video: false,
        };
        if (deviceId) constraints.audio.deviceId = { exact: deviceId };
        return await navigator.mediaDevices.getUserMedia(constraints);
    }

    function buildGraph(stream) {
        if (micStream) micStream.getTracks().forEach(t => t.stop());
        micStream = stream;
        if (!audioCtx) audioCtx = new (window.AudioContext || window.webkitAudioContext)();
        if (audioCtx.state === 'suspended') audioCtx.resume();
        sampleRate = audioCtx.sampleRate;
        const src = audioCtx.createMediaStreamSource(stream);
        analyser = audioCtx.createAnalyser();
        analyser.fftSize = FFT_SIZE;
        analyser.smoothingTimeConstant = 0.5;
        freqData = new Float32Array(analyser.frequencyBinCount);
        src.connect(analyser); // not connected to destination → no feedback
        webAudioOk = true;
    }

    // Shared with the player page: the player's Input dropdown calls this
    // with its own <select> so both pickers list the same devices and
    // preselect the same one (native engine device, else USB/guitar-ish).
    async function populateDevicePicker(select) {
        const el = select || elDevice;
        if (!el) return;
        try {
            const devs = await navigator.mediaDevices.enumerateDevices();
            const inputs = devs.filter(d => d.kind === 'audioinput');
            // Try to preselect the device the Audio Engine is configured for.
            let preselect = '';
            try {
                const cur = window.slopsmithDesktop && window.slopsmithDesktop.audio
                    ? await window.slopsmithDesktop.audio.getCurrentDevice() : null;
                if (cur) {
                    const m = inputs.find(d => (d.label && d.label.includes(cur)) || (cur.includes(d.label || ' ')));
                    if (m) preselect = m.deviceId;
                }
            } catch { /* ignore */ }
            // No native engine to match (plain Chrome): prefer a
            // USB/Rocksmith/guitar-looking device over the built-in mic —
            // the mic reads as digital silence for an electric guitar and
            // makes Free Play look dead.
            if (!preselect) {
                const m = inputs.find(d => d.label && /usb|rocksmith|guitar|audio device/i.test(d.label));
                if (m) preselect = m.deviceId;
            }
            el.innerHTML = '<option value="">Default</option>' + inputs.map(d =>
                `<option value="${d.deviceId}"${d.deviceId === preselect ? ' selected' : ''}>${d.label || ('Device ' + (d.deviceId || '').slice(0, 6))}</option>`).join('');
        } catch { /* ignore */ }
    }

    async function setupWebAudio() {
        try {
            const stream = await acquireStream(null); // default first to obtain permission + labels
            buildGraph(stream);
            await populateDevicePicker();
            if (elDevice && elDevice.value) {
                // A USB/guitar device was preselected — capture it right
                // away instead of leaving the stream on the default (often
                // the built-in mic).
                await switchDevice(elDevice.value);
            }
            console.log('Free Play: Web Audio capture initialized', {
                sampleRate: audioCtx.sampleRate,
                fftSize: FFT_SIZE,
                device: micStream && micStream.getAudioTracks().length
                    ? micStream.getAudioTracks()[0].label : (elDevice && elDevice.value ? elDevice.value : 'default'),
            });
        } catch (e) {
            console.warn('Free Play: Web Audio capture failed, falling back to JUCE pitch detection', e);
            webAudioOk = false;
        }
    }

    async function switchDevice(deviceId) {
        if (!deviceId) { await setupWebAudio(); return; }
        try {
            buildGraph(await acquireStream(deviceId));
            console.log('Free Play: input device switched to', micStream.getAudioTracks()[0].label);
        } catch (e) {
            console.warn('Free Play: device switch failed, re-enumerating', e);
            // Stale deviceId after a re-plug is the common cause — the
            // device re-enumerates with a new ID and the exact match
            // throws. Refresh the list and retry by label.
            try {
                const devs = await navigator.mediaDevices.enumerateDevices();
                const old = devs.find(d => d.deviceId === deviceId);
                const m = old && old.label
                    ? devs.find(d => d.kind === 'audioinput' && d.deviceId !== deviceId && d.label === old.label)
                    : null;
                if (m) {
                    buildGraph(await acquireStream(m.deviceId));
                    console.log('Free Play: input device switched to', micStream.getAudioTracks()[0].label, '(after re-enumeration)');
                } else {
                    console.error('Free Play: no device matches the failed switch target');
                }
            } catch (e2) {
                console.error('Free Play: device switch retry failed', e2);
            }
        }
    }

    // ── UI controls ──────────────────────────────────────────────────────
    function toggleMirror() {
        mirrored = !mirrored;
        if (elMirrorBtn) {
            elMirrorBtn.classList.toggle('bg-accent', mirrored);
            elMirrorBtn.classList.toggle('text-white', mirrored);
        }
    }

    function toggleTilt() {
        tilted = !tilted;
        if (elTiltBtn) {
            elTiltBtn.classList.toggle('bg-accent', tilted);
            elTiltBtn.classList.toggle('text-white', tilted);
        }
    }

    // ── Metronome (standalone click track, adjustable BPM) ────────────────
    // Sample-accurate lookahead scheduler (Chris Wilson's "A Tale of Two
    // Clocks" pattern). A frequent setInterval pump does NOT itself make
    // sound — that would drift (each fire is relative to the last, so error
    // accumulates) and jitter (main-thread timers are delayed by GC, layout,
    // the detection loop, etc.). Instead the pump only enqueues clicks a
    // short distance ahead at exact points on the AudioContext.currentTime
    // timeline, which is driven by the audio hardware clock. Result: rock-
    // steady clicks even while the UI thread is busy.
    let metroOn = false;
    let metroTimer = null;            // setInterval pump handle
    let metroBeat = 0;                // beat counter (accent every 4th)
    let metroBpm = 120;
    let metroNextTime = 0;            // next beat's time on the audio clock
    let metroClickCtx = null;         // AudioContext for the click track
    const METRO_SCHEDULE_AHEAD = 0.1; // s — how far ahead to pre-schedule
    const METRO_LOOKAHEAD_MS = 25;    // ms — pump interval

    function metroCtx() {
        // Reuse the mic AudioContext when available (keeps one clock);
        // otherwise create/refresh a dedicated one for the click track.
        if (!metroClickCtx || metroClickCtx.state === 'closed') {
            const AC = window.AudioContext || window.webkitAudioContext;
            metroClickCtx = (audioCtx && audioCtx.state !== 'closed') ? audioCtx : new AC();
        }
        if (metroClickCtx.state === 'suspended') { try { metroClickCtx.resume(); } catch { /* ignore */ } }
        return metroClickCtx;
    }

    // Schedule a single click at an exact audio-clock time (not "now").
    function scheduleClick(when, accent) {
        const ctx = metroClickCtx;
        if (!ctx) return;
        const osc = ctx.createOscillator();
        const gain = ctx.createGain();
        osc.connect(gain);
        gain.connect(ctx.destination);
        osc.frequency.value = accent ? 1200 : 800;
        osc.type = 'sine';
        // Short percussive envelope. Guard the ramp start above zero —
        // exponentialRampToValueAtTime throws on a 0 start value.
        gain.gain.setValueAtTime(0.0001, when);
        gain.gain.exponentialRampToValueAtTime(0.5, when + 0.002);
        gain.gain.exponentialRampToValueAtTime(0.001, when + 0.08);
        osc.start(when);
        osc.stop(when + 0.09);
    }

    function metroScheduler() {
        const ctx = metroCtx();
        if (!ctx) return;
        const secondsPerBeat = 60 / metroBpm;
        // Enqueue every beat that falls inside the lookahead window.
        while (metroNextTime < ctx.currentTime + METRO_SCHEDULE_AHEAD) {
            scheduleClick(metroNextTime, metroBeat % 4 === 0);
            metroNextTime += secondsPerBeat;
            metroBeat++;
        }
    }

    function metroRestart() {
        if (metroTimer) clearInterval(metroTimer);
        const ctx = metroCtx();
        if (!ctx) return;
        metroBeat = 0;
        // Start slightly ahead of "now" so the first click is never late.
        metroNextTime = ctx.currentTime + 0.06;
        metroScheduler();                 // prime the first window immediately
        metroTimer = setInterval(metroScheduler, METRO_LOOKAHEAD_MS);
    }

    function metroStop() {
        if (metroTimer) { clearInterval(metroTimer); metroTimer = null; }
    }

    function toggleMetronome() {
        metroOn = !metroOn;
        const btn = document.getElementById('fp-metronome');
        if (btn) {
            btn.classList.toggle('bg-accent', metroOn);
            btn.classList.toggle('text-white', metroOn);
        }
        if (metroOn) metroRestart();
        else metroStop();
    }

    function setMetronomeBpm(value) {
        metroBpm = Math.max(30, Math.min(300, parseInt(value, 10) || 120));
        const input = document.getElementById('fp-metronome-bpm');
        if (input) input.value = metroBpm;
        // No restart needed: the scheduler reads `metroBpm` live, so the new
        // tempo takes effect from the next beat without an audible gap.
    }

    // ── Recording / playback controls ────────────────────────────────────
    function finalizeRecording(now) {
        if (!recording) return;
        for (const entry of activeNotes.values()) closeNote(entry, now);
        activeNotes.clear();
        recording = false;
        stopMediaRecorder();
    }

    function toggleRecord() {
        if (recording) {
            finalizeRecording(performance.now() / 1000);
        } else {
            playback = null;
            session = []; sessionDur = 0; audioBlob = null;
            recording = true; recStart = performance.now() / 1000;
            startMediaRecorder();
        }
        updateRecButtons();
    }
    function togglePlay() {
        if (playback) { playback = null; }
        else if (session.length && !recording) { playback = { start: performance.now() / 1000 }; }
        updateRecButtons();
    }
    function toggleLoop() { loopOn = !loopOn; updateRecButtons(); }
    function clearSession() {
        session = []; sessionDur = 0; playback = null; recording = false;
        audioBlob = null;
        if (mediaRecorder && mediaRecorder.state !== 'inactive') {
            try { mediaRecorder.stop(); } catch { /* ignore */ }
        }
        updateRecButtons();
    }

    // ── Audio capture (MediaRecorder on the mic stream) ──────────────────
    function startMediaRecorder() {
        recChunks = []; audioBlob = null; recMime = '';
        if (!micStream || typeof MediaRecorder === 'undefined') { mediaRecorder = null; return; }
        const candidates = ['audio/webm;codecs=opus', 'audio/webm', 'audio/ogg;codecs=opus'];
        const mime = candidates.find(m => { try { return MediaRecorder.isTypeSupported(m); } catch { return false; } });
        try {
            mediaRecorder = new MediaRecorder(micStream, mime ? { mimeType: mime } : undefined);
            recMime = mime || 'audio/webm';
            mediaRecorder.ondataavailable = (e) => { if (e.data && e.data.size) recChunks.push(e.data); };
            mediaRecorder.onstop = () => {
                if (recChunks.length) audioBlob = new Blob(recChunks, { type: recMime });
                updateRecButtons();
            };
            mediaRecorder.start();
        } catch (e) {
            console.warn('Free Play: MediaRecorder failed, audio export unavailable', e);
            mediaRecorder = null;
        }
    }
    function stopMediaRecorder() {
        if (mediaRecorder && mediaRecorder.state !== 'inactive') {
            try { mediaRecorder.stop(); } catch { /* ignore */ }
        }
    }

    // ── Export ───────────────────────────────────────────────────────────
    function downloadBlob(blob, name) {
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url; a.download = name;
        document.body.appendChild(a); a.click();
        document.body.removeChild(a);
        setTimeout(() => URL.revokeObjectURL(url), 2000);
    }

    // Variable-length quantity (MIDI SMF delta-time encoding).
    function vlq(n) {
        const out = [n & 0x7f];
        n >>= 7;
        while (n > 0) { out.unshift((n & 0x7f) | 0x80); n >>= 7; }
        return out;
    }

    function exportMidi() {
        if (!session.length) return;
        const tpq = 480;                 // ticks per quarter
        const usPerQ = 500000;          // 120 BPM
        const ticksPerSec = tpq * 1e6 / usPerQ; // 960
        const midiOf = (n) => (n.midi != null ? n.midi : openMidi(n.s) + n.f);

        // Build event list: tempo + program at t=0, then note on/off pairs.
        const evs = [
            { tick: 0, data: [0xff, 0x51, 0x03, (usPerQ >> 16) & 0xff, (usPerQ >> 8) & 0xff, usPerQ & 0xff] }, // tempo
            { tick: 0, data: [0xc0, 25] }, // Program Change ch0 → Acoustic Guitar (steel)
        ];
        for (const n of session) {
            const note = midiOf(n);
            if (note < 0 || note > 127) continue;
            const on = Math.max(0, Math.round(n.t * ticksPerSec));
            const off = Math.max(on + 1, Math.round((n.t + n.sus) * ticksPerSec));
            evs.push({ tick: on, data: [0x90, note, 100] });  // Note On ch0
            evs.push({ tick: off, data: [0x80, note, 0] });   // Note Off ch0
        }
        evs.sort((a, b) => a.tick - b.tick);

        const track = [];
        let last = 0;
        for (const e of evs) {
            track.push(...vlq(e.tick - last), ...e.data);
            last = e.tick;
        }
        track.push(...vlq(0), 0xff, 0x2f, 0x00); // End of Track

        const header = [0x4d, 0x54, 0x68, 0x64, 0, 0, 0, 6, 0, 0, 0, 1, (tpq >> 8) & 0xff, tpq & 0xff]; // MThd, format 0, 1 track
        const out = new Uint8Array(header.length + 8 + track.length);
        out.set(header, 0);
        const t0 = header.length;
        out[t0] = 0x4d; out[t0 + 1] = 0x54; out[t0 + 2] = 0x72; out[t0 + 3] = 0x6b; // MTrk
        const len = track.length;
        out[t0 + 4] = (len >> 24) & 0xff; out[t0 + 5] = (len >> 16) & 0xff;
        out[t0 + 6] = (len >> 8) & 0xff; out[t0 + 7] = len & 0xff;
        out.set(track, t0 + 8);
        downloadBlob(new Blob([out], { type: 'audio/midi' }), 'freeplay.mid');
    }

    function exportAudio() {
        if (!audioBlob) return;
        const ext = /webm/.test(recMime) ? 'webm' : /ogg/.test(recMime) ? 'ogg' : 'audio';
        downloadBlob(audioBlob, 'freeplay.' + ext);
    }

    // ── Main loop ────────────────────────────────────────────────────────
    async function tick() {
        if (!running) return;
        rafId = requestAnimationFrame(tick);
        const now = performance.now() / 1000;

        if (now * 1000 - lastPollAt >= POLL_INTERVAL_MS && !pollInFlight) {
            lastPollAt = now * 1000;
            pollInFlight = true;
            let detected = [];
            try { detected = await detect(now); }
            catch { detected = []; }
            pollInFlight = false;

            if (detected.length) lastHeardAt = now;
            applyDetected(detected, now);
            updateHud(detected);
        }

        notes = notes.filter(n => (now - n.t) < CULL_SECONDS);
        draw(now);
        updateOverlay(now);
        if (elRecTime && (recording || playback)) updateRecButtons();
    }

    // ── Lifecycle ────────────────────────────────────────────────────────
    // Named listener (not an anonymous closure) so start()/stop() can
    // add/remove it symmetrically — repeated Learn toggles would otherwise
    // accumulate dead listeners.
    function onDeviceChange() {
        switchDevice(elDevice.value);
        // Keep the player page's copy of the dropdown in sync.
        const playerSel = document.getElementById('player-device');
        if (playerSel) playerSel.value = elDevice.value;
    }

    async function start() {
        if (running) return;
        canvas = $('freeplay-canvas');
        elNote = $('fp-note'); elFreq = $('fp-freq'); elPos = $('fp-pos');
        elOverlay = $('freeplay-overlay'); elOverlayTitle = $('fp-overlay-title'); elOverlayBody = $('fp-overlay-body');
        elTuning = $('fp-tuning'); elCapo = $('fp-capo'); elDevice = $('fp-device');
        elRec = $('fp-rec'); elPlay = $('fp-play'); elLoopBtn = $('fp-loop'); elClear = $('fp-clear'); elRecTime = $('fp-rec-time');
        elExpMidi = $('fp-exp-midi'); elExpAudio = $('fp-exp-audio');
        elRootNote = $('fp-root-note'); elRootFreq = $('fp-root-freq');
        elMirrorBtn = $('fp-mirror'); elTiltBtn = $('fp-tilt');
        if (!canvas) return;
        ctx = canvas.getContext('2d');
        resize();
        window.addEventListener('resize', resize);
        if (elDevice) elDevice.addEventListener('change', onDeviceChange);

        notes = []; activeNotes.clear(); pending.clear(); session = []; sessionDur = 0; playback = null;
        lastHeardAt = performance.now() / 1000;

        // Primary path: Web Audio. The native engine is only needed for
        // the monophonic fallback, but probe it so the overlay can tell
        // the user nothing is available at all.
        const audio = window.slopsmithDesktop && window.slopsmithDesktop.audio;
        try { audioAvailable = audio ? !!(await audio.isAvailable()) : false; } catch { audioAvailable = false; }
        await setupWebAudio();
        console.log('Free Play: input path =', webAudioOk
            ? 'Web Audio (polyphonic)'
            : (audioAvailable ? 'JUCE fallback (monophonic)' : 'NONE — no audio input'));
        updateRecButtons();

        // Restore the metronome if the user left it on before navigating away
        // (stop() preserves metroOn but halts the pump).
        if (metroOn && (webAudioOk || audioAvailable)) metroRestart();

        running = true; lastPollAt = 0;
        tick();
    }

    function stop() {
        running = false;
        if (rafId) { cancelAnimationFrame(rafId); rafId = null; }
        window.removeEventListener('resize', resize);
        if (elDevice) elDevice.removeEventListener('change', onDeviceChange);
        // Finalize any in-flight recording so its audio blob completes.
        finalizeRecording(performance.now() / 1000);
        notes = []; activeNotes.clear(); pending.clear();
        playback = null;
        // Release the mic so the indicator turns off when leaving Free Play.
        if (micStream) { micStream.getTracks().forEach(t => t.stop()); micStream = null; }
        // Stop the metronome pump BEFORE suspending audioCtx — the scheduler
        // would otherwise keep calling metroCtx(), resuming the suspended mic
        // context and clicking on whatever screen the user navigated to. The
        // metroOn flag is preserved so start() restores it.
        metroStop();
        if (audioCtx && audioCtx.state !== 'closed') { try { audioCtx.suspend(); } catch { /* ignore */ } }
        // Clear the Learn-mode song reference so Free Play returns to its
        // own tuning/capo selectors.
        _refOffsets = null; _refCapo = null;
    }

    window.freeplay = {
        start, stop, toggleRecord, togglePlay, toggleLoop, clearSession, exportMidi,
        exportAudio, toggleMirror, toggleTilt, toggleMetronome, setMetronomeBpm,
        populateDevicePicker, switchDevice,
        // Learn-mode hooks: what's currently ringing (post onset-debounce),
        // whether any input path is usable, and the song tuning/capo pin.
        getDetected() {
            const out = [];
            for (const e of activeNotes.values()) out.push({ s: e.s, f: e.f, midi: e.midi });
            return out;
        },
        inputAvailable() { return webAudioOk || audioAvailable; },
        setSongReference(offsets, capo) {
            _refOffsets = (offsets && Array.isArray(offsets) && offsets.length)
                ? offsets.slice(0, 6) : null;
            _refCapo = Number.isFinite(capo) && capo > 0 ? capo : null;
        },
    };
    // Debug interface
    window.freeplay.__debug = {
        get webAudioOk() { return webAudioOk; },
        get analyser() { return analyser; },
        get sampleRate() { return sampleRate; },
        get audioCtx() { return audioCtx; },
        get activeNotes() { return Array.from(activeNotes.entries()); },
        // Pick-attack (spectral-flux) detector observability.
        get onset() { return _frameOnset; },
        get repick() { return _frameRepick; },
        get flux() { return _frameFlux; },
        get fluxEma() { return _fluxEma; },
    };
})();
