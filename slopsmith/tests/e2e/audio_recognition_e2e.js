#!/usr/bin/env node
/**
 * End-to-end test for the wired-guitar polyphonic recognition pipeline.
 *
 * Exercises the REAL scorer in static/freeplay.js (via the __debug.runDetection
 * hook) against synthesized guitar-like signals, with no physical guitar and
 * no live AudioContext:
 *
 *   synth time-domain pluck(s)  ->  Hann window + radix-2 FFT  ->  dB spectrum
 *   (shaped like AnalyserNode.getFloatFrequencyData)  ->  runDetection  ->
 *   detected (string, fret) set  ->  compared to ground truth.
 *
 * Covers: single notes across all 6 strings x frets, open/strummed chords,
 * a power chord, silence / noise-only (false-positive) frames, and the
 * spectral-flux re-pick (onset) flag. Prints precision/recall and exits
 * non-zero when accuracy thresholds regress, so it doubles as a guard when
 * tuning the detection constants.
 *
 * Run:  node slopsmith/tests/e2e/audio_recognition_e2e.js
 */
'use strict';
const fs = require('fs');
const path = require('path');

const FREEPLAY = path.join(__dirname, '..', '..', 'static', 'freeplay.js');

// ── Load freeplay.js headlessly ────────────────────────────────────────────
global.window = {};
// eslint-disable-next-line no-eval
eval(fs.readFileSync(FREEPLAY, 'utf8'));
const fp = global.window.freeplay;
if (!fp || !fp.__debug || typeof fp.__debug.runDetection !== 'function') {
    console.error('FAIL: freeplay __debug.runDetection hook missing');
    process.exit(1);
}
const runDetection = (fd, sr) => fp.__debug.runDetection(fd, sr);

// ── Constants mirroring freeplay.js ────────────────────────────────────────
const BASE_STD = [40, 45, 50, 55, 59, 64]; // s=0..5 low E .. high e
const SR = 48000;
const N = 16384;                 // FFT size used by freeplay
const BINS = N / 2;

// ── Deterministic RNG ──────────────────────────────────────────────────────
let _seed = 123456789;
function rand() { _seed = (_seed * 1103515245 + 12345) & 0x7fffffff; return _seed / 0x7fffffff; }

// ── Radix-2 iterative FFT ──────────────────────────────────────────────────
function fftMagDB(x) {
    const n = N;
    const re = new Float64Array(n), im = new Float64Array(n);
    // Hann window (emulates the analyser's windowing)
    for (let i = 0; i < n; i++) re[i] = x[i] * (0.5 - 0.5 * Math.cos(2 * Math.PI * i / (n - 1)));
    // bit-reversal permutation
    for (let i = 1, j = 0; i < n; i++) {
        let bit = n >> 1;
        for (; j & bit; bit >>= 1) j ^= bit;
        j ^= bit;
        if (i < j) { const t = re[i]; re[i] = re[j]; re[j] = t; const t2 = im[i]; im[i] = im[j]; im[j] = t2; }
    }
    for (let len = 2; len <= n; len <<= 1) {
        const ang = -2 * Math.PI / len;
        const wr = Math.cos(ang), wi = Math.sin(ang);
        for (let i = 0; i < n; i += len) {
            let cwr = 1, cwi = 0;
            for (let k = 0; k < len / 2; k++) {
                const ur = re[i + k], ui = im[i + k];
                const vr = re[i + k + len / 2] * cwr - im[i + k + len / 2] * cwi;
                const vi = re[i + k + len / 2] * cwi + im[i + k + len / 2] * cwr;
                re[i + k] = ur + vr; im[i + k] = ui + vi;
                re[i + k + len / 2] = ur - vr; im[i + k + len / 2] = ui - vi;
                const nwr = cwr * wr - cwi * wi; cwi = cwr * wi + cwi * wr; cwr = nwr;
            }
        }
    }
    // magnitude -> dBFS, normalised so a full-scale sine reads ~0 dB
    const db = new Float32Array(BINS);
    for (let k = 0; k < BINS; k++) {
        const mag = Math.sqrt(re[k] * re[k] + im[k] * im[k]) / (N / 4);
        db[k] = 20 * Math.log10(mag + 1e-12);
    }
    return db;
}

// ── Guitar-tone synthesizer ────────────────────────────────────────────────
// notes: [{midi, amp}] ; adds a decaying harmonic series + a noise floor.
function synthFrame(notes, { noiseDb = -95, transient = 0 } = {}) {
    const x = new Float64Array(N);
    for (const nt of notes) {
        const f0 = 440 * Math.pow(2, (nt.midi - 69) / 12);
        const A = nt.amp;
        for (let h = 1; h <= 12; h++) {
            const f = f0 * h;
            if (f > SR * 0.45) break;
            const amp = A / Math.pow(h, 1.15);
            const ph = h * 0.7;
            const w = 2 * Math.PI * f / SR;
            for (let i = 0; i < N; i++) x[i] += amp * Math.sin(w * i + ph);
        }
    }
    const noiseAmp = Math.pow(10, noiseDb / 20);
    for (let i = 0; i < N; i++) x[i] += noiseAmp * (rand() * 2 - 1);
    if (transient > 0) {
        // broadband pick attack over the first ~2 ms
        const L = Math.floor(SR * 0.002);
        for (let i = 0; i < L; i++) x[i] += transient * (rand() * 2 - 1) * (1 - i / L);
    }
    return x;
}
const midiOf = (s, f) => BASE_STD[s] + f;

// ── Metrics helpers ────────────────────────────────────────────────────────
const key = (d) => d.s + ':' + d.f;
function compare(detected, truth) {
    const ds = new Set(detected.map(key)), ts = new Set(truth.map(key));
    let tp = 0; for (const k of ds) if (ts.has(k)) tp++;
    return { tp, fp: ds.size - tp, fn: ts.size - tp, detected, truth };
}

let failures = 0;
function check(cond, label, extra) {
    if (cond) { console.log('  ok   ' + label); }
    else { failures++; console.log('  FAIL ' + label + (extra ? ' :: ' + extra : '')); }
}

// ── 1) Single notes: all strings x representative frets ────────────────────
console.log('\n[1] single-note recognition (6 strings x frets)');
const frets = [0, 2, 3, 5, 7, 9, 12];
let snTotal = 0, snPitchOk = 0, snStringOk = 0; const snMiss = [];
for (let s = 0; s < 6; s++) {
    for (const f of frets) {
        const target = midiOf(s, f);
        const db = fftMagDB(synthFrame([{ midi: target, amp: 0.2 }]));
        const det = runDetection(db, SR);
        snTotal++;
        // Pitch is what audio can determine; the string is ambiguous when the
        // same MIDI is playable on several strings (identical spectrum).
        const pitchHit = det.some(d => d.midi === target);
        const stringHit = det.length === 1 && det[0].s === s && det[0].f === f;
        if (pitchHit) snPitchOk++;
        if (stringHit) snStringOk++;
        if (!pitchHit) snMiss.push(`s${s}f${f}(midi${target})->${JSON.stringify(det.map(key))}`);
    }
}
const snAcc = snPitchOk / snTotal;
console.log(`  pitch accuracy = ${(snAcc * 100).toFixed(1)}% (${snPitchOk}/${snTotal}); string-exact = ${(snStringOk / snTotal * 100).toFixed(1)}% (info)`);
if (snMiss.length) console.log('  pitch misses: ' + snMiss.slice(0, 8).join(', '));
check(snAcc >= 0.95, 'single-note pitch accuracy >= 95%', snMiss.slice(0, 8).join(', '));

// ── 2) Chords ──────────────────────────────────────────────────────────────
console.log('\n[2] chord recognition');
// NOTE: single-frame magnitude polyphony cannot reliably separate a stacked
// octave from the lower note's own 2nd harmonic (near-identical spectra, and
// synthetic phase cancellation makes it worse), so octave-heavy voicings cap
// recall ~0.5-0.7. The 0.5 bar guards against regressions below current level.
const chords = [
    { name: 'E maj', notes: [[0, 0], [1, 2], [2, 2], [3, 1], [4, 0], [5, 0]] },
    { name: 'A min', notes: [[1, 0], [2, 2], [3, 2], [4, 1], [5, 0]] },
    { name: 'C maj', notes: [[1, 3], [2, 2], [3, 0], [4, 1], [5, 0]] },
    { name: 'G maj', notes: [[0, 3], [1, 2], [2, 0], [3, 0], [4, 0], [5, 3]] },
    { name: 'E5 power', notes: [[0, 2], [1, 4], [2, 4]] },
];
let chRecallSum = 0, chPrecSum = 0;
for (const ch of chords) {
    // vary per-string amplitude a little to mimic real strum dynamics
    const notes = ch.notes.map(([s, f], i) => ({ midi: midiOf(s, f), amp: 0.18 + 0.05 * ((i * 37) % 3) / 2 }));
    const db = fftMagDB(synthFrame(notes));
    const det = runDetection(db, SR);
    // Compare by pitch set (string of a given pitch can be ambiguous).
    const dm = new Set(det.map(d => d.midi)), tm = new Set(ch.notes.map(([s, f]) => midiOf(s, f)));
    let tp = 0; for (const m of dm) if (tm.has(m)) tp++;
    const recall = tp / tm.size, prec = tp / Math.max(1, dm.size);
    chRecallSum += recall; chPrecSum += prec;
    console.log(`  ${ch.name.padEnd(9)} recall=${recall.toFixed(2)} precision=${prec.toFixed(2)} detMidi=${JSON.stringify([...dm])} wantMidi=${JSON.stringify([...tm])}`);
    check(recall >= 0.5, `${ch.name} recall >= 0.5`, JSON.stringify([...dm]));
    check(prec >= 0.8, `${ch.name} precision >= 0.8`, JSON.stringify([...dm]));
}
console.log(`  mean recall=${(chRecallSum / chords.length).toFixed(2)} mean precision=${(chPrecSum / chords.length).toFixed(2)}`);

// ── 3) Silence / noise-only: no false positives ────────────────────────────
console.log('\n[3] silence / noise-only (false positives)');
for (const nd of [-95, -88]) {
    const db = fftMagDB(synthFrame([], { noiseDb: nd }));
    const det = runDetection(db, SR);
    check(det.length === 0, `noise ${nd} dB -> 0 detections`, JSON.stringify(det.map(key)));
}

// ── 4) Re-pick (onset) flag ────────────────────────────────────────────────
console.log('\n[4] pick-attack (re-pick) onset flag');
{
    // Prime: feed the SAME full-amplitude spectrum repeatedly so inter-frame
    // flux is exactly 0 and the flux EMA settles to ~0 (a single noise->tone
    // jump inflates it and only decays 0.9x/frame). Then steady repick=false.
    const dbFull = fftMagDB(synthFrame([{ midi: midiOf(0, 3), amp: 0.2 }]));
    for (let i = 0; i < 60; i++) runDetection(dbFull, SR);
    const steadyRepick = fp.__debug.repick;
    // Real re-pick: previous frame is the decayed (quiet) note, current frame
    // jumps back to full amplitude -> large per-bin flux spike above the
    // settled gate.
    runDetection(fftMagDB(synthFrame([{ midi: midiOf(0, 3), amp: 0.04 }])), SR);
    runDetection(dbFull, SR);
    const attackRepick = fp.__debug.repick;
    check(steadyRepick === false, 'steady sustain -> repick false');
    check(attackRepick === true, 'decayed->full re-pick -> repick true');
}

// ── Summary ────────────────────────────────────────────────────────────────
console.log('\n' + (failures === 0 ? 'E2E AUDIO RECOGNITION: ALL CHECKS PASSED' : `E2E AUDIO RECOGNITION: ${failures} CHECK(S) FAILED`));
process.exit(failures === 0 ? 0 : 1);
