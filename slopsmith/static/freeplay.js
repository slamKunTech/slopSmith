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
    const SILENCE_OVERLAY_MS = 2000;

    // Polyphonic detection params
    const FFT_SIZE = 8192;
    const HARMONICS = 5;
    const FUND_WEIGHT = 1.0;
    const HARM_WEIGHT = 0.5;
    const ABS_FLOOR_DB = -85;           // below this a bin is treated as silence
    const REL_GATE = 0.12;             // a fundamental must be ≥12% of this frame's peak

    // ── State ────────────────────────────────────────────────────────────
    let canvas = null, ctx = null, rafId = null, running = false;
    let lastPollAt = 0, pollInFlight = false;
    let lastHeardAt = 0;
    let audioAvailable = false;        // JUCE engine present (fallback path)

    // Web Audio graph
    let audioCtx = null, analyser = null, freqData = null, micStream = null;
    let webAudioOk = false, sampleRate = 48000;

    // Note pipeline (shared by polyphonic + fallback)
    let notes = [];                    // closed notes: {t, s, f, sus}  (t = abs seconds)
    let activeNotes = new Map();       // key "s:f" -> {t, s, f, midi}  (currently ringing)

    // Recording / playback
    let recording = false, recStart = 0;
    let session = [];                  // [{t, s, f, sus}]  (t = seconds from recStart)
    let sessionDur = 0;
    let playback = null;               // {start, loop} or null
    let loopOn = false;

    // Recording export
    let mediaRecorder = null, recChunks = [], audioBlob = null, recMime = '';

    // HUD element refs
    let elNote, elFreq, elPos, elOverlay, elOverlayTitle, elOverlayBody;
    let elTuning, elCapo, elDevice, elRec, elPlay, elLoopBtn, elClear, elRecTime;
    let elExpMidi, elExpAudio;

    function $(id) { return document.getElementById(id); }

    // ── Tuning / capo helpers ────────────────────────────────────────────
    function currentOffsets() { return TUNINGS[elTuning ? elTuning.value : 'std-E'] || TUNINGS['std-E']; }
    function currentCapo() {
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
        const off = (s - (STRING_COUNT - 1) / 2) / ((STRING_COUNT - 1) / 2);
        return pr.W / 2 + off * pr.halfW;
    }

    // ── Drawing ──────────────────────────────────────────────────────────
    function drawBackground() {
        const W = canvas.width, H = canvas.height;
        const top = project(1), bot = project(0);
        const cx = W / 2;
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
        const linAt = (bin) => {
            if (bin < 0 || bin >= freqData.length) return 0;
            const db = freqData[bin];
            if (!isFinite(db) || db <= ABS_FLOOR_DB) return 0;
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
                let score = FUND_WEIGHT * fund;
                for (let h = 2; h <= HARMONICS; h++) score += HARM_WEIGHT * linAt(Math.round((f0 * h) / binHz));
                scored.push({ s, f, midi, score, fund });
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
            let best = null;
            for (let i = 0; i < arr.length; i++) {
                const c = arr[i];
                if (c.fund < gate) continue;
                const prev = arr[i - 1], next = arr[i + 1];
                const localMax = (!prev || c.score >= prev.score) && (!next || c.score >= next.score);
                if (!localMax) continue;
                if (!best || c.score > best.score) best = c;
            }
            perString[s] = best;
        }

        // Cross-string dedup + harmonic-ghost suppression.
        let winners = perString.filter(Boolean);
        // Suppress overtone ghosts: a winner whose fundamental sits on an
        // integer-multiple harmonic of a louder winner, AND is much weaker
        // than it, is almost certainly an overtone rather than a second note.
        // The much-weaker guard (≤50%) keeps real octaves — which usually
        // have comparable fundamental energy — from being dropped.
        winners.sort((a, b) => b.score - a.score);
        const kept = [];
        for (const c of winners) {
            const f0 = 440 * Math.pow(2, (c.midi - 69) / 12);
            let ghost = false;
            for (const k of kept) {
                const kf0 = 440 * Math.pow(2, (k.midi - 69) / 12);
                const ratio = f0 / kf0;
                if (ratio > 1.9 && ratio < 6.1) {
                    const n = Math.round(ratio);
                    if (Math.abs(ratio - n) < 0.03 && c.fund < k.fund * 0.5) { ghost = true; break; }
                }
            }
            if (!ghost) kept.push(c);
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
            if (!activeNotes.has(k)) activeNotes.set(k, { t: now, s: d.s, f: d.f, midi: d.midi });
        }
        for (const [k, entry] of activeNotes) {
            if (!keys.has(k)) { closeNote(entry, now); activeNotes.delete(k); }
        }
    }

    // ── HUD + overlay ────────────────────────────────────────────────────
    function updateHud(detected) {
        if (!elNote) return;
        if (detected.length) {
            elNote.textContent = detected.map(d => midiToName(d.midi)).join(' ');
            elFreq.textContent = '';
            elPos.textContent = detected.map(d => STRING_NAMES[d.s] + d.f).join(' · ');
        } else {
            elNote.textContent = '—'; elFreq.textContent = ''; elPos.textContent = '';
        }
    }

    function updateOverlay(now) {
        if (!elOverlay) return;
        let show = false, title = 'Waiting for guitar input…';
        let body = 'Plug in your Rocksmith cable and pick it in the Input dropdown. Free Play captures it directly via Web Audio.';
        if (!webAudioOk && !audioAvailable) {
            show = true; title = 'No audio input available';
            body = 'Web Audio capture failed and the native audio engine is unavailable. Plug in your Rocksmith cable and retry.';
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

    async function populateDevicePicker() {
        if (!elDevice) return;
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
            elDevice.innerHTML = '<option value="">Default</option>' + inputs.map(d =>
                `<option value="${d.deviceId}"${d.deviceId === preselect ? ' selected' : ''}>${d.label || ('Device ' + (d.deviceId || '').slice(0, 6))}</option>`).join('');
        } catch { /* ignore */ }
    }

    async function setupWebAudio() {
        try {
            const stream = await acquireStream(null); // default first to obtain permission + labels
            buildGraph(stream);
            await populateDevicePicker();
        } catch (e) {
            console.warn('Free Play: Web Audio capture failed, falling back to JUCE pitch detection', e);
            webAudioOk = false;
        }
    }

    async function switchDevice(deviceId) {
        if (!deviceId) { await setupWebAudio(); return; }
        try { buildGraph(await acquireStream(deviceId)); }
        catch (e) { console.warn('Free Play: device switch failed', e); }
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
    async function start() {
        if (running) return;
        canvas = $('freeplay-canvas');
        elNote = $('fp-note'); elFreq = $('fp-freq'); elPos = $('fp-pos');
        elOverlay = $('freeplay-overlay'); elOverlayTitle = $('fp-overlay-title'); elOverlayBody = $('fp-overlay-body');
        elTuning = $('fp-tuning'); elCapo = $('fp-capo'); elDevice = $('fp-device');
        elRec = $('fp-rec'); elPlay = $('fp-play'); elLoopBtn = $('fp-loop'); elClear = $('fp-clear'); elRecTime = $('fp-rec-time');
        elExpMidi = $('fp-exp-midi'); elExpAudio = $('fp-exp-audio');
        if (!canvas) return;
        ctx = canvas.getContext('2d');
        resize();
        window.addEventListener('resize', resize);
        if (elDevice) elDevice.addEventListener('change', () => switchDevice(elDevice.value));

        notes = []; activeNotes.clear(); session = []; sessionDur = 0; playback = null;
        lastHeardAt = performance.now() / 1000;

        // Primary path: Web Audio. The native engine is only needed for
        // the monophonic fallback, but probe it so the overlay can tell
        // the user nothing is available at all.
        const audio = window.slopsmithDesktop && window.slopsmithDesktop.audio;
        try { audioAvailable = audio ? !!(await audio.isAvailable()) : false; } catch { audioAvailable = false; }
        await setupWebAudio();
        updateRecButtons();

        running = true; lastPollAt = 0;
        tick();
    }

    function stop() {
        running = false;
        if (rafId) { cancelAnimationFrame(rafId); rafId = null; }
        window.removeEventListener('resize', resize);
        // Finalize any in-flight recording so its audio blob completes.
        finalizeRecording(performance.now() / 1000);
        notes = []; activeNotes.clear();
        playback = null;
        // Release the mic so the indicator turns off when leaving Free Play.
        if (micStream) { micStream.getTracks().forEach(t => t.stop()); micStream = null; }
        if (audioCtx && audioCtx.state !== 'closed') { try { audioCtx.suspend(); } catch { /* ignore */ } }
    }

    window.freeplay = { start, stop, toggleRecord, togglePlay, toggleLoop, clearSession, exportMidi, exportAudio };
})();
