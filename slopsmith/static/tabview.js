// ── Tablature renderer (built-in visualization) ─────────────────────────────
// A traditional 6-line guitar tablature view that scrolls in sync with
// playback, generated live from the sloppak/PSARC chart data the highway
// already streams (note/chord string+fret+time+sustain + technique flags).
//
// It plugs into the existing setRenderer contract (see highway.js / CLAUDE.md
// "Visualization plugins"): a factory on window.slopsmithViz_tab returns
// { init, draw, resize, destroy } and draws on the SAME 2D canvas the default
// highway uses — so switching Highway ⇄ Tab never hits the "first context
// wins" canvas lock (both are 2D).
//
// Design notes:
//   • Time flows left→right; a fixed "play line" sits ~28% from the left.
//     x(t) = playX + (t - currentTime) * PPS. Future notes enter from the
//     right, cross the play line, exit left. Linear (not perspective) scroll
//     matches how real tab is read.
//   • String order: top line = highest-pitched string (standard tab). The
//     data uses s=0 for the LOW string, so lineFromTop(s) = stringCount-1-s.
//     bundle.inverted flips that; bundle.lefty mirrors x.
//   • Only notes/chords inside the visible time window are drawn, located via
//     binary search (the arrays are time-sorted server-side).
//   • draw() is fully guarded — it never throws, so the highway's
//     auto-revert-on-repeated-failure can't be tripped by a data edge case.
(function () {
    'use strict';

    const NOTE_NAMES = ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B'];
    const BASE_STD = [40, 45, 50, 55, 59, 64]; // s=0..5 → E2 A2 D3 G3 B3 E4
    // Per-string colours, indexed by string s (low→high). Mirrors the
    // highway's palette so the two views feel consistent.
    const STRING_COLORS = ['#e05252', '#e08b3b', '#d8c34a', '#4fbf6f', '#4a9be0', '#9b6fe0'];

    const LOOKAHEAD_SEC = 3.6;   // seconds of future visible right of the play line
    const PLAY_X_FRAC = 0.28;    // play-line position (fraction of width)

    function createTabRenderer() {
        let cv = null, ctx = null;
        let W = 0, H = 0;

        // Binary search: first index in arr whose .t >= target (arr sorted by .t).
        function lowerBound(arr, target) {
            let lo = 0, hi = arr.length;
            while (lo < hi) {
                const mid = (lo + hi) >> 1;
                if ((arr[mid].t || 0) < target) lo = mid + 1;
                else hi = mid;
            }
            return lo;
        }

        function stringColor(s, stringCount) {
            // Map any string index into the 6-colour palette; extended-range
            // (7/8-string) charts fall back to a neutral hue beyond index 5.
            if (s >= 0 && s < STRING_COLORS.length) return STRING_COLORS[s];
            return '#8a8aa0';
        }

        function lineY(s, stringCount, inverted, topPad, gap) {
            const fromTop = inverted ? s : (stringCount - 1 - s);
            return topPad + fromTop * gap;
        }

        // Compute the note-name labels for the left gutter from the song's
        // tuning offsets + capo (so alternate tunings read correctly).
        function stringLabels(songInfo, stringCount) {
            const tuning = (songInfo && Array.isArray(songInfo.tuning)) ? songInfo.tuning : [];
            const capo = (songInfo && Number(songInfo.capo)) || 0;
            const out = [];
            for (let s = 0; s < stringCount; s++) {
                const base = s < BASE_STD.length ? BASE_STD[s] : (BASE_STD[BASE_STD.length - 1] + 5 * (s - BASE_STD.length + 1));
                const midi = base + (tuning[s] || 0) + capo;
                out.push(midi >= 0 ? NOTE_NAMES[((midi % 12) + 12) % 12] + (Math.floor(midi / 12) - 1) : '?');
            }
            return out;
        }

        function resize(w, h) {
            if (w) W = w;
            if (h) H = h;
        }

        function draw(bundle) {
            try {
                if (!ctx || !cv) return;
                const b = bundle || {};
                const stringCount = Math.max(1, b.stringCount || 6);
                const currentTime = b.currentTime || 0;
                const inverted = !!b.inverted;
                const lefty = !!b.lefty;
                const notes = b.notes || [];
                const chords = b.chords || [];
                const beats = b.beats || [];
                const templates = b.chordTemplates || [];

                // Layout
                const topPad = H * 0.14;
                const botPad = H * 0.12;
                const usable = Math.max(40, H - topPad - botPad);
                const gap = stringCount > 1 ? usable / (stringCount - 1) : 0;
                const playX = W * PLAY_X_FRAC;
                const futureW = Math.max(1, W - playX);
                const PPS = futureW / LOOKAHEAD_SEC;      // pixels per second
                const pastSec = playX / PPS;              // seconds visible left of play line

                const mapX = (t) => {
                    const x = playX + (t - currentTime) * PPS;
                    return lefty ? (W - x) : x;
                };

                // Background
                ctx.fillStyle = '#080810';
                ctx.fillRect(0, 0, W, H);

                const tMin = currentTime - pastSec - 0.3;
                const tMax = currentTime + LOOKAHEAD_SEC + 0.3;

                // ── Bar / beat lines ──────────────────────────────────────
                if (beats.length) {
                    // beats entries are {time, measure}; binary-search the
                    // window start (they arrive time-sorted) then scan forward.
                    let lo = 0, hi = beats.length;
                    while (lo < hi) {
                        const mid = (lo + hi) >> 1;
                        if ((beats[mid].time || 0) < tMin) lo = mid + 1;
                        else hi = mid;
                    }
                    for (let i = lo; i < beats.length; i++) {
                        const bt = beats[i].time;
                        if (bt < tMin) continue;
                        if (bt > tMax) break;
                        const x = mapX(bt);
                        const downbeat = beats[i].measure >= 0;
                        ctx.strokeStyle = downbeat ? 'rgba(255,255,255,0.28)' : 'rgba(255,255,255,0.10)';
                        ctx.lineWidth = downbeat ? 2 : 1;
                        ctx.beginPath();
                        ctx.moveTo(x, topPad - gap * 0.4);
                        ctx.lineTo(x, topPad + (stringCount - 1) * gap + gap * 0.4);
                        ctx.stroke();
                    }
                }

                // ── String lines ──────────────────────────────────────────
                for (let s = 0; s < stringCount; s++) {
                    const y = lineY(s, stringCount, inverted, topPad, gap);
                    ctx.strokeStyle = s === 0 ? 'rgba(190,190,210,0.55)' : 'rgba(150,150,180,0.40)';
                    ctx.lineWidth = s === 0 ? 2 : 1.3;
                    ctx.beginPath();
                    ctx.moveTo(0, y);
                    ctx.lineTo(W, y);
                    ctx.stroke();
                }

                const r = Math.min(gap * 0.40, 15);         // note-head radius
                const numFont = `${Math.max(9, Math.round(r * 1.05))}px ui-sans-serif, system-ui`;
                const techFont = `${Math.max(8, Math.round(r * 0.72))}px ui-sans-serif, system-ui`;

                // Helper: draw one note head (+ sustain + technique marker).
                function drawNote(n, t, alpha) {
                    const s = n.s, f = n.f;
                    if (s == null || s < 0 || s >= stringCount) return;
                    const x = mapX(t);
                    if (x < -40 || x > W + 40) return;
                    const y = lineY(s, stringCount, inverted, topPad, gap);
                    const col = stringColor(s, stringCount);

                    // Sustain bar
                    const sus = n.sus || 0;
                    if (sus > 0.02) {
                        const x2 = mapX(t + sus);
                        ctx.globalAlpha = 0.5 * alpha;
                        ctx.strokeStyle = col;
                        ctx.lineWidth = Math.max(2, r * 0.5);
                        ctx.beginPath();
                        ctx.moveTo(x, y);
                        ctx.lineTo(x2, y);
                        ctx.stroke();
                        ctx.globalAlpha = alpha;
                    }

                    // Active highlight when the play line is inside the note
                    const active = currentTime >= t - 0.06 && currentTime <= t + sus + 0.06;

                    // Note head
                    ctx.globalAlpha = alpha;
                    ctx.beginPath();
                    ctx.arc(x, y, r, 0, Math.PI * 2);
                    ctx.fillStyle = active ? '#ffffff' : col;
                    ctx.fill();
                    if (active) {
                        ctx.strokeStyle = col; ctx.lineWidth = 3;
                        ctx.beginPath(); ctx.arc(x, y, r + 3, 0, Math.PI * 2); ctx.stroke();
                    }
                    // Fret number
                    ctx.fillStyle = active ? '#08080f' : '#0b0b14';
                    ctx.font = numFont;
                    ctx.fillText(String(f), x, y + 0.5);

                    // Technique marker (pick the most salient one)
                    let mark = '';
                    if (n.bn) mark = (n.bn > 0 ? '↑' : '') + (n.bn >= 1.5 ? '1½' : n.bn >= 1 ? '1' : n.bn >= 0.5 ? '½' : '');
                    else if (n.sl != null && n.sl >= 0) mark = (n.sl > f ? '/' : '\\');
                    else if (n.hm || n.hp) mark = n.hp ? 'PIH' : 'harm';
                    else if (n.ho) mark = 'h';
                    else if (n.po) mark = 'p';
                    else if (n.tp) mark = 'T';
                    if (mark) {
                        ctx.fillStyle = 'rgba(230,230,240,0.85)';
                        ctx.font = techFont;
                        ctx.textAlign = lefty ? 'right' : 'left';
                        ctx.fillText(mark, x + (lefty ? -(r + 3) : (r + 3)), y - r * 0.15);
                        ctx.textAlign = 'center';
                    }
                    // Palm-mute dot above the head
                    if (n.pm) {
                        ctx.fillStyle = 'rgba(230,230,240,0.7)';
                        ctx.font = techFont;
                        ctx.fillText('PM', x, y - r - 6);
                    }
                    ctx.globalAlpha = 1;
                }

                // ── Single notes ──────────────────────────────────────────
                if (notes.length) {
                    let i = lowerBound(notes, tMin);
                    // include notes whose sustain started before tMin but still ringing
                    while (i > 0 && (notes[i - 1].t || 0) + (notes[i - 1].sus || 0) > tMin) i--;
                    for (; i < notes.length; i++) {
                        const n = notes[i];
                        if ((n.t || 0) > tMax) break;
                        const age = currentTime - (n.t || 0);
                        const alpha = age > 0 ? Math.max(0.35, 1 - age / (pastSec + 0.5)) : 1;
                        drawNote(n, n.t || 0, alpha);
                    }
                }

                // ── Chords ────────────────────────────────────────────────
                if (chords.length) {
                    let i = lowerBound(chords, tMin);
                    for (; i < chords.length; i++) {
                        const ch = chords[i];
                        const t = ch.t || 0;
                        if (t > tMax) break;
                        const cn = ch.notes || [];
                        if (!cn.length) continue;
                        const age = currentTime - t;
                        const alpha = age > 0 ? Math.max(0.4, 1 - age / (pastSec + 0.5)) : 1;

                        // Bracket spanning the chord's strings
                        let minS = cn[0].s, maxS = cn[0].s;
                        for (const m of cn) { if (m.s < minS) minS = m.s; if (m.s > maxS) maxS = m.s; }
                        const x = mapX(t);
                        if (x > -60 && x < W + 60) {
                            const yTop = lineY(maxS, stringCount, inverted, topPad, gap);
                            const yBot = lineY(minS, stringCount, inverted, topPad, gap);
                            ctx.globalAlpha = 0.5 * alpha;
                            ctx.strokeStyle = '#cfd2ff';
                            ctx.lineWidth = 2;
                            ctx.beginPath();
                            ctx.moveTo(x, yTop - r);
                            ctx.lineTo(x, yBot + r);
                            ctx.stroke();
                            ctx.globalAlpha = alpha;
                            // Chord name (from templates) above the top line
                            const tpl = templates[ch.id];
                            if (tpl && tpl.name) {
                                ctx.fillStyle = 'rgba(207,210,255,0.9)';
                                ctx.font = `${Math.max(10, Math.round(r * 0.95))}px ui-sans-serif, system-ui`;
                                ctx.textAlign = 'center';
                                ctx.fillText(tpl.name, x, Math.min(yTop, yBot) - r - 10);
                            }
                        }
                        for (const m of cn) drawNote(m, t, alpha);
                        ctx.globalAlpha = 1;
                    }
                }

                // ── Play line ─────────────────────────────────────────────
                const px = lefty ? (W - playX) : playX;
                const grad = ctx.createLinearGradient(px, topPad - gap, px, topPad + (stringCount - 1) * gap + gap);
                grad.addColorStop(0, 'rgba(255,255,255,0.05)');
                grad.addColorStop(0.5, 'rgba(255,255,255,0.85)');
                grad.addColorStop(1, 'rgba(255,255,255,0.05)');
                ctx.strokeStyle = grad;
                ctx.lineWidth = 2.5;
                ctx.beginPath();
                ctx.moveTo(px, topPad - gap * 0.7);
                ctx.lineTo(px, topPad + (stringCount - 1) * gap + gap * 0.7);
                ctx.stroke();

                // ── String labels (top layer, never clipped) ──────────────
                // Drawn last so they sit above faded past notes. Left-aligned
                // at a small inset (right-aligned when lefty) with a capped
                // font and a translucent chip, so 2-3 char labels like "C#4"
                // are always fully visible at the canvas edge.
                const labels = stringLabels(b.songInfo, stringCount);
                const labFont = Math.max(10, Math.min(22, Math.round(gap * 0.42)));
                ctx.font = `${labFont}px ui-sans-serif, system-ui`;
                ctx.textBaseline = 'middle';
                ctx.textAlign = lefty ? 'right' : 'left';
                const labX = lefty ? (W - 8) : 8;
                for (let s = 0; s < stringCount; s++) {
                    const y = lineY(s, stringCount, inverted, topPad, gap);
                    const txt = labels[s];
                    const tw = ctx.measureText(txt).width;
                    const chipX = lefty ? (labX - tw - 4) : (labX - 4);
                    ctx.fillStyle = 'rgba(8,8,16,0.72)';
                    ctx.fillRect(chipX, y - labFont * 0.62, tw + 8, labFont * 1.24);
                    ctx.fillStyle = stringColor(s, stringCount);
                    ctx.fillText(txt, labX, y);
                }

                // "TAB" gutter hint
                ctx.fillStyle = 'rgba(255,255,255,0.25)';
                ctx.font = `${Math.max(10, Math.round(gap * 0.5))}px ui-sans-serif, system-ui`;
                ctx.textAlign = lefty ? 'right' : 'left';
                ctx.fillText('TAB', lefty ? (W - 6) : 6, topPad - gap * 1.1);
                ctx.textAlign = 'center';
            } catch (e) {
                // Never let a data edge-case throw out of draw() — the highway
                // counts consecutive throws and would auto-revert the renderer.
                if (!draw._warned) { console.warn('tab renderer draw error:', e); draw._warned = true; }
            }
        }

        return {
            init(canvas /*, bundle */) {
                cv = canvas;
                ctx = canvas.getContext('2d');
                W = canvas.width; H = canvas.height;
            },
            draw,
            resize,
            destroy() { cv = null; ctx = null; },
        };
    }

    window.createTabRenderer = createTabRenderer;
    // Register as a built-in viz so the player's Visualization picker
    // (setViz('tab')) and localStorage restore both find it. No
    // matchesArrangement → Auto mode never picks it implicitly; it's an
    // explicit user choice, which is the intent for a notation view.
    window.slopsmithViz_tab = createTabRenderer;
})();
