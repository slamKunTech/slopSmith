/**
 * Canvas-based Rocksmith highway renderer.
 * Receives note data via WebSocket, renders on requestAnimationFrame.
 */
function createHighway() {
    let canvas, ctx, ws;
    // Two notions of "now" — kept deliberately separate:
    //   chartTime — audio-aligned clock. What getTime() exposes to plugins
    //               (scoring, note detection, etc.) and what setTime() receives.
    //   currentTime — rendering clock. Equal to chartTime + avOffsetSec, so the
    //                 draw code can shift visual notes forward to compensate
    //                 for audio-output pipeline latency without plugins having
    //                 to care about the offset.
    // avOffsetSec is set by setAvOffset(ms); default 0 means old behavior.
    let chartTime = 0;
    let currentTime = 0;
    let avOffsetSec = 0;
    let animFrame = null;
    let _connectOpts = {};
    let _resizeContainer = null;
    let _resizeHandler = null;
    let _onLyricsChange = null;

    // Song data (populated via WebSocket)
    let songInfo = {};
    let notes = [];
    let chords = [];
    let beats = [];
    let sections = [];
    let anchors = [];
    let chordTemplates = [];
    // Number of strings on the active arrangement. Updated from the
    // `stringCount` field in each `song_info` WS message; falls back
    // to `tuning.length` (works for older servers that don't yet emit
    // stringCount) then to 6 (final safety). 4 = bass, 6 = guitar,
    // 7+ = extended-range GP imports.
    let stringCount = 6;
    let lyrics = [];
    let toneChanges = [];
    let toneBase = "";
    let ready = false;
    // Master-difficulty (slopsmith#48). _phrases stays null as a
    // "slider disabled" sentinel when the source chart has no ladder
    // data (GP imports, legacy sloppak) — the server omits the
    // `phrases` message entirely in that case. When populated, the
    // filter maps the slider fraction to a per-phrase level index and
    // stages _filteredNotes / _filteredChords for the render loop.
    // _filteredNotes === null means "fall through to flat notes" —
    // either no phrase data or filter not rebuilt yet.
    let _phrases = null;
    // Default to full chart. Persistence lives in the caller (app.js
    // loadSettings, or a splitscreen plugin managing its own panel
    // state) so multiple createHighway() instances stay truly
    // per-instance — no shared localStorage key to race on.
    let _mastery = 1;
    let _filteredNotes = null;
    let _filteredChords = null;
    let _filteredAnchors = null;
    let showLyrics = localStorage.getItem('showLyrics') !== 'false';
    let _drawHooks = [];  // plugin draw callbacks: fn(ctx, W, H)
    let _renderScale = parseFloat(localStorage.getItem('renderScale') || '1');  // 1 = full, 0.5 = half res
    let _inverted = localStorage.getItem('invertHighway') === 'true';
    let _lefty = localStorage.getItem('lefty') === '1';
    let _lastChordOnFretLine = null;  // chord object currently shown on fret line
    let _chordFretLineNotes = [];  // notes to render on fret line
    const _frameMismatchWarned = new Set();  // chord ids already warned about (slopsmith#88)
    // Per-chord render info, computed lazily once per src array (slopsmith#88).
    const _chordRenderInfo = new WeakMap();  // chord -> { chainIndex, chainLen, isFull, baseFret }
    let _chordRenderCacheSrc = null;
    let _chordRenderCacheInverted = null;

    // Rendering config
    const VISIBLE_SECONDS = 3.0;
    const Z_CAM = 2.2;
    const Z_MAX = 10.0;
    const BG = '#080810';

    // String color palettes. Indices 0–5 cover guitar / bass; 6–7
    // are added for extended-range GP imports (7-string, 8-string).
    // Lookups still use `|| '#888'` as a safety fallback for any
    // out-of-range index.
    const STRING_COLORS = [
        '#cc0000', '#cca800', '#0066cc',
        '#cc6600', '#00cc66', '#9900cc',
        '#cc00aa', '#00cccc',  // 7th = magenta, 8th = teal
    ];
    const STRING_DIM = [
        '#520000', '#524200', '#002952',
        '#522900', '#005229', '#3d0052',
        '#520042', '#005252',
    ];
    const STRING_BRIGHT = [
        '#ff3c3c', '#ffe040', '#3c9cff',
        '#ff9c3c', '#3cff9c', '#cc3cff',
        '#ff3ce0', '#3ce0e0',
    ];

    // ── Projection ───────────────────────────────────────────────────────
    function project(tOffset) {
        if (tOffset > VISIBLE_SECONDS || tOffset < -0.05) return null;
        if (tOffset < 0) return { y: 0.82 + Math.abs(tOffset) * 0.3, scale: 1.0 };

        const z = tOffset * (Z_MAX / VISIBLE_SECONDS);
        const denom = z + Z_CAM;
        if (denom < 0.01) return null;
        const scale = Z_CAM / denom;
        const y = 0.82 + (0.08 - 0.82) * (1.0 - scale);
        return { y, scale };
    }

    // ── Anchor / Fret mapping ────────────────────────────────────────────
    // Zoom approach: fret 0 at the left edge, fret N at the right (entire canvas mirrored when lefty).
    // The "zoom level" determines how many frets are visible.
    // When playing low frets, zoom in (fewer frets visible, bigger notes).
    // When playing high frets, zoom out (more frets visible, smaller spacing).
    let displayMaxFret = 12;  // rightmost visible fret (smoothed)

    function getAnchorAt(t) {
        // Same master-difficulty fallback as the render loops — the
        // anchor ladder pairs with the note ladder.
        const src = _filteredAnchors !== null ? _filteredAnchors : anchors;
        let a = src[0] || { fret: 1, width: 4 };
        for (const anc of src) {
            if (anc.time > t) break;
            a = anc;
        }
        return a;
    }

    function getMaxFretInWindow(t) {
        // Find the highest fret needed across all anchors visible on screen
        const src = _filteredAnchors !== null ? _filteredAnchors : anchors;
        let maxFret = 0;
        for (const anc of src) {
            if (anc.time > t + VISIBLE_SECONDS + 2) break; // Skip anchors well in the future (with a little buffer to avoid moving early the cutoff)
            if (anc.time + 2 < t) continue;  // skip anchors well in the past
            const top = anc.fret + anc.width;
            if (top > maxFret) maxFret = top;
        }
        return maxFret;
    }

    function updateSmoothAnchor(anchor, dt) {
        // Smoothing rate balances two regressions seen in slopsmith#88:
        //   rate=1.0 (was) snapped to target every frame — visible jitter
        //   on aerial passages where anchors moved every few frames.
        //   rate=0.15 (Knaifhogg) was too gentle — large jumps (low frets
        //   to teens) took ~3s to catch up, pushing upcoming notes off the
        //   right edge.
        // 0.4 splits the difference: half-life ~1.7s, but the per-frame
        // step at 60fps is ~0.0067 — still small enough that frame-to-frame
        // changes read as smooth.
        const rate = Math.min(0.4 * dt, 0.4);
        // Look ahead: use the widest fret range across all visible anchors
        const lookAheadMax = getMaxFretInWindow(currentTime);
        const currentMax = anchor.fret + anchor.width;
        const needed = Math.max(currentMax, lookAheadMax);
        const targetMax = Math.max(needed + 3, 8);
        displayMaxFret += (targetMax - displayMaxFret) * rate;
    }

    function fretX(fret, scale, w) {
        const hw = w * 0.52 * scale;
        const margin = hw * 0.06;
        const usable = hw * 2 - 2 * margin;
        const t = fret / Math.max(1, displayMaxFret);
        return w / 2 - hw + margin + t * usable;
    }

    /** Call while lefty mirror transform is active; keeps glyphs readable. */
    function fillTextReadable(text, x, y) {
        // ctx may be null when the 2D context was never acquired
        // (canvas already locked to WebGL). No-op in that case —
        // alternatives would be throwing, which breaks plugin hooks
        // that call this after a context-type mismatch.
        if (!canvas || !ctx) return;
        const W = canvas.width;
        if (!_lefty) {
            ctx.fillText(text, x, y);
            return;
        }
        ctx.save();
        ctx.setTransform(1, 0, 0, 1, 0, 0);
        ctx.fillText(text, W - x, y);
        ctx.restore();
    }

    // ── Drawing ──────────────────────────────────────────────────────────
    //
    // slopsmith#36 — swappable renderers.
    //
    // The default renderer below is the original 2D canvas highway. Its
    // methods still reach into the factory closure (ctx, beats, notes,
    // _drawHooks, etc.) to avoid rewriting every helper; it's not
    // "isolated," just shaped as the contract. Custom renderers from
    // plugins (3D, tab, fretboard, future "keys"/"drums") pass through
    // setRenderer() and consume the bundle instead of the closure —
    // they stay self-contained and never touch the factory's `ctx`.
    //
    // Lifecycle: setRenderer(r) -> previous.destroy() -> r.init(canvas,
    // bundle) -> per frame r.draw(bundle) -> on resize r.resize(w, h) ->
    // on stop or swap r.destroy(). Renderer owns its rendering context
    // (2D, WebGL, DOM overlay). Factory owns canvas element, rAF, WS,
    // data state, resize subscription, _drawHooks for 2D compositing.
    //
    // Contract for setRenderer(r): r is an object with at minimum
    // {draw(bundle)}. init / resize / destroy are optional. Pass null
    // or undefined to restore the default renderer.
    //
    // The bundle (see _makeBundle) is a per-frame snapshot of factory
    // state — includes difficulty-filtered note / chord / anchor arrays
    // so renderers never touch _filteredX internals directly. Arrays
    // are live references (performance), NOT copies — renderers must
    // treat them as read-only.
    let _renderer = null;

    function _makeBundle() {
        // Snapshot of current factory state passed to each renderer call.
        // Arrays and songInfo are LIVE references, not copies — the bundle
        // itself is rebuilt each frame but its `notes`, `chords`,
        // `anchors`, `beats`, etc. point at closure state. Renderers
        // MUST NOT mutate these; treat them as read-only. We don't
        // Object.freeze or deep-copy for per-frame allocation cost reasons.
        return {
            // Timing
            currentTime,
            songInfo,
            isReady: ready,

            // Chart content (filter-aware — difficulty-filtered arrays
            // preferred; raw arrays are the fallback when no ladder data).
            notes: _filteredNotes !== null ? _filteredNotes : notes,
            chords: _filteredChords !== null ? _filteredChords : chords,
            anchors: _filteredAnchors !== null ? _filteredAnchors : anchors,
            beats,
            sections,
            chordTemplates,
            stringCount,
            lyrics,
            toneChanges,
            toneBase,

            // Master-difficulty (slopsmith#48)
            mastery: _mastery,
            hasPhraseData: !!(_phrases && _phrases.length > 0),

            // Display flags
            inverted: _inverted,
            lefty: _lefty,
            renderScale: _renderScale,
            lyricsVisible: showLyrics,

            // 2D-style helpers (renderers that don't need these can ignore).
            // `fillTextUnmirrored` is deliberately NOT exposed here —
            // the factory-level version writes to the default renderer's
            // closure ctx, which is null for custom renderers. Renderers
            // that need lefty-aware text should check `bundle.lefty` and
            // apply the mirror transform themselves on their own context.
            project,
            fretX,
        };
    }

    const _defaultRenderer = {
        _ctxWarned: false,
        init(canvasEl /* , bundle */) {
            // getContext('2d') returns null when the canvas is already
            // locked to another context type (e.g. a WebGL viz plugin
            // grabbed it first). Once that happens the 2D renderer can't
            // recover on the same canvas — surface a single clear error
            // and skip drawing. A future revision will recreate the
            // canvas element on renderer-type swap to avoid this.
            ctx = canvasEl.getContext('2d');
            if (!ctx && !this._ctxWarned) {
                console.error(
                    'Default 2D renderer: canvas.getContext("2d") returned null ' +
                    '— the canvas is locked to another context type. ' +
                    'Reload the page to restore the highway.'
                );
                this._ctxWarned = true;
            }
        },
        draw(/* bundle */) {
            // Still reads from the factory closure directly — the bundle
            // is shaped for custom renderers, not used here. Keeping the
            // default renderer's body unchanged from the pre-refactor
            // draw() preserves pixel-level parity with current main.
            if (!canvas || !ready || !ctx) return;
            try {
                const W = canvas.width;
                const H = canvas.height;
                ctx.fillStyle = BG;
                ctx.fillRect(0, 0, W, H);

                const anchor = getAnchorAt(currentTime);
                updateSmoothAnchor(anchor, 1 / 60);

                ctx.save();
                if (_lefty) {
                    ctx.translate(W, 0);
                    ctx.scale(-1, 1);
                }

                drawHighway(W, H);
                drawFretLines(W, H);
                drawBeats(W, H);
                drawStrings(W, H);
                drawSustains(W, H);
                drawNowLine(W, H);
                drawNotes(W, H);
                drawChords(W, H);
                drawFretNumbers(W, H);

                // Plugin draw hooks (same coordinate system as the highway).
                // Hooks are a 2D-only contract — the default renderer owns
                // their invocation. Custom renderers on non-2D contexts
                // (e.g. WebGL) don't call them; the factory doesn't
                // invoke hooks on their behalf.
                for (const hook of _drawHooks) {
                    try { hook(ctx, W, H); } catch (e) { /* ignore */ }
                }

                ctx.restore();

                // Lyrics: drawn unmirrored so lines stay left-to-right readable (layout is center-symmetric)
                if (showLyrics) drawLyrics(W, H);
            } catch (e) {
                console.error('draw error:', e);
            }
        },
        resize(/* w, h */) {
            // no-op; canvas dimension change is handled by the factory,
            // and the 2D context doesn't maintain persistent state we'd
            // need to rebuild here.
        },
        destroy() {
            // Leave ctx intact. Helper paths like fillTextReadable /
            // api.fillTextUnmirrored may still be called while another
            // renderer is active or after stop() (e.g. a residual draw
            // hook, plugin cleanup code). Forcing ctx to null would
            // make those calls throw. A subsequent init() re-assigns
            // ctx via canvasEl.getContext('2d') — the browser returns
            // the same cached context for the same canvas, so there's
            // nothing to "refresh" by nulling. Reset the warn-once
            // guard so a fresh init on a fresh canvas is a new
            // opportunity to succeed or fail.
            this._ctxWarned = false;
        },
    };

    // Tracks consecutive renderer.draw failures so a permanently broken
    // renderer auto-reverts to default instead of spamming the console
    // every frame. Reset on every successful draw and whenever a new
    // renderer is installed.
    let _rendererDrawFailures = 0;
    const MAX_RENDERER_DRAW_FAILURES = 3;

    // True only while the current renderer has had a successful init
    // since its last destroy (or was freshly installed but never init'd
    // because canvas was null). Gates destroy calls so an uninit'd
    // renderer doesn't receive spurious destroys — the restore-on-
    // page-load flow relies on this: setRenderer can run before init.
    let _rendererInited = false;

    function _destroyCurrentIfInited() {
        if (_renderer && _rendererInited && typeof _renderer.destroy === 'function') {
            try { _renderer.destroy(); }
            catch (e) { console.error('renderer destroy:', e); }
        }
        _rendererInited = false;
    }

    function _emitVizReverted(reason) {
        // Notify listeners (e.g. app.js's viz picker, splitscreen's
        // per-panel picker in Wave C) that the factory auto-reverted
        // to the default renderer — so the UI / persisted selection
        // don't keep advertising the broken plugin.
        if (window.slopsmith && typeof window.slopsmith.emit === 'function') {
            try { window.slopsmith.emit('viz:reverted', { reason }); }
            catch (e) { console.error('viz:reverted emit:', e); }
        }
    }

    function _setRenderer(r) {
        _destroyCurrentIfInited();
        // null/undefined reverts to default. Anything else must provide
        // at minimum a draw(bundle) function — without it the rAF loop
        // would throw every frame. Log once and fall back to default
        // rather than accepting a broken renderer.
        let next;
        if (r == null) {
            next = _defaultRenderer;
        } else if (typeof r.draw === 'function') {
            next = r;
        } else {
            console.error('setRenderer: renderer missing draw(bundle) function; reverting to default.');
            next = _defaultRenderer;
        }
        _renderer = next;
        _rendererDrawFailures = 0;
        // Defer init/resize until the canvas is available. setRenderer
        // can legitimately be called before api.init() runs (e.g. app.js
        // restoring a saved picker selection at page load, before any
        // song has been played). api.init() will re-run these when it
        // assigns the canvas.
        if (!canvas) return;
        const bundle = _makeBundle();
        // A renderer without an init() function is treated as ready
        // by default (it simply has no setup to do). If an init()
        // exists, only flip the flag true when it returns without
        // throwing — otherwise a later destroy would run on an
        // effectively-uninitialized renderer.
        let initSucceeded = typeof _renderer.init !== 'function';
        if (typeof _renderer.init === 'function') {
            try {
                _renderer.init(canvas, bundle);
                initSucceeded = true;
            }
            catch (e) {
                console.error('renderer init:', e);
                // Init may have partially allocated GPU/DOM resources
                // before throwing. Run destroy best-effort to release
                // whatever it got — renderer's destroy contract already
                // requires handling partial state gracefully. Then
                // revert to the default renderer so the user isn't
                // stranded on a broken viz, and notify the UI so the
                // picker + localStorage sync back to 'default'.
                if (_renderer !== _defaultRenderer) {
                    if (typeof _renderer.destroy === 'function') {
                        try { _renderer.destroy(); }
                        catch (destroyErr) {
                            console.error('renderer destroy after init failure:', destroyErr);
                        }
                    }
                    _renderer = _defaultRenderer;
                    _emitVizReverted('init-failure');
                    if (typeof _renderer.init === 'function') {
                        try {
                            _renderer.init(canvas, _makeBundle());
                            initSucceeded = true;
                        }
                        catch (e2) {
                            console.error('default renderer init after revert:', e2);
                        }
                    } else {
                        initSucceeded = true;
                    }
                }
            }
        }
        _rendererInited = initSucceeded;
        if (!_rendererInited) return;
        if (typeof _renderer.resize === 'function') {
            try { _renderer.resize(canvas.width, canvas.height); }
            catch (e) { console.error('renderer resize:', e); }
        }
    }

    function draw() {
        animFrame = requestAnimationFrame(draw);
        if (!canvas || !_renderer) return;
        // Match pre-refactor behaviour: skip draw until WS ready fires.
        // This gates out the brief "arrays cleared, WS reconnecting"
        // window during playSong / reconnect. Renderers that want to
        // draw a loading state can still opt in via the `isReady`
        // field on the bundle passed to a custom pre-ready handler —
        // we'd need to widen the contract to support that, out of
        // scope here. Default 2D renderer also checks `ready` in its
        // draw body (defence in depth).
        if (!ready) return;
        // Skip bundle allocation when the default renderer is active —
        // it reads closure state directly and ignores the bundle.
        // _makeBundle at 60fps was a steady GC churn for the common
        // case where no custom renderer is installed.
        const bundle = _renderer === _defaultRenderer ? undefined : _makeBundle();
        try {
            _renderer.draw(bundle);
            _rendererDrawFailures = 0;
        } catch (e) {
            _rendererDrawFailures += 1;
            console.error('renderer draw:', e);
            // Self-heal: a plugin whose draw() throws every frame
            // would otherwise spam the console and leave the canvas
            // blank indefinitely. After a short streak of failures,
            // revert to the built-in renderer so the user at least
            // gets the default highway back. 2D default is known-safe.
            if (_rendererDrawFailures >= MAX_RENDERER_DRAW_FAILURES &&
                _renderer !== _defaultRenderer) {
                console.error(
                    'renderer draw: failed ' + _rendererDrawFailures +
                    ' frames in a row; reverting to default renderer.'
                );
                _setRenderer(_defaultRenderer);
                _emitVizReverted('draw-failure');
            }
        }
    }

    function drawHighway(W, H) {
        const strips = 40;
        for (let i = 0; i < strips; i++) {
            const t0 = (i / strips) * VISIBLE_SECONDS;
            const t1 = ((i + 1) / strips) * VISIBLE_SECONDS;
            const p0 = project(t0), p1 = project(t1);
            if (!p0 || !p1) continue;

            const hw0 = W * 0.26 * p0.scale;
            const hw1 = W * 0.26 * p1.scale;
            const bright = 18 + 10 * p0.scale;

            ctx.fillStyle = `rgb(${bright|0},${bright|0},${(bright+14)|0})`;
            ctx.beginPath();
            ctx.moveTo(W/2 - hw0, p0.y * H);
            ctx.lineTo(W/2 + hw0, p0.y * H);
            ctx.lineTo(W/2 + hw1, p1.y * H);
            ctx.lineTo(W/2 - hw1, p1.y * H);
            ctx.fill();
        }
    }

    function drawFretLines(W, H) {
        const pad = 3;
        const lo = 0;
        const hi = Math.ceil(displayMaxFret);
        ctx.strokeStyle = '#2d2d45';
        ctx.lineWidth = 1;

        for (let fret = lo; fret <= hi; fret++) {
            if (fret < 0) continue;
            ctx.beginPath();
            for (let i = 0; i <= 40; i++) {
                const t = (i / 40) * VISIBLE_SECONDS;
                const p = project(t);
                if (!p) continue;
                const x = fretX(fret, p.scale, W);
                if (i === 0) ctx.moveTo(x, p.y * H);
                else ctx.lineTo(x, p.y * H);
            }
            ctx.stroke();
        }
    }

    function drawBeats(W, H) {
        for (const beat of beats) {
            const tOff = beat.time - currentTime;
            const p = project(tOff);
            if (!p || p.scale < 0.06) continue;
            const hw = W * 0.26 * p.scale;
            const isMeasure = beat.measure >= 0;
            ctx.strokeStyle = isMeasure ? '#343450' : '#202038';
            ctx.lineWidth = isMeasure ? 2 : 1;
            ctx.beginPath();
            ctx.moveTo(W/2 - hw, p.y * H);
            ctx.lineTo(W/2 + hw, p.y * H);
            ctx.stroke();
        }
    }

    function drawStrings(W, H) {
        const strTop = H * 0.83;
        const strBot = H * 0.95;
        const margin = W * 0.03;
        // Adapt to the active arrangement's string count: 4 for bass,
        // 6 for guitar, 7+ for extended-range GP imports. The visible
        // band [strTop..strBot] gets divided into (stringCount - 1)
        // slots, so 4 strings spread across the full band rather than
        // using the upper 4/6ths of the 6-string layout. The Math.max
        // guards against a hypothetical 1-string instrument (denom=0).
        const span = Math.max(1, stringCount - 1);
        for (let i = 0; i < stringCount; i++) {
            const yi = _inverted ? (stringCount - 1 - i) : i;
            const y = strTop + (yi / span) * (strBot - strTop);
            ctx.strokeStyle = STRING_COLORS[i] || '#888';
            ctx.lineWidth = 3;
            ctx.beginPath();
            ctx.moveTo(margin, y);
            ctx.lineTo(W - margin, y);
            ctx.stroke();
        }
    }

    function drawNowLine(W, H) {
        const y = H * 0.82;
        const hw = W * 0.26;
        // Glow
        for (let i = 1; i < 5; i++) {
            const a = Math.max(0, 70 - i * 15);
            ctx.strokeStyle = `rgba(${a},${a},${a+8},1)`;
            ctx.lineWidth = 1;
            ctx.beginPath();
            ctx.moveTo(W/2 - hw, y - i);
            ctx.lineTo(W/2 + hw, y - i);
            ctx.stroke();
            ctx.beginPath();
            ctx.moveTo(W/2 - hw, y + i);
            ctx.lineTo(W/2 + hw, y + i);
            ctx.stroke();
        }
        ctx.strokeStyle = '#dce0f0';
        ctx.lineWidth = 2;
        ctx.beginPath();
        ctx.moveTo(W/2 - hw, y);
        ctx.lineTo(W/2 + hw, y);
        ctx.stroke();
    }

    function drawNote(W, H, x, y, scale, string, fret, opts) {
        const isHarmonic = opts?.hm || opts?.hp || false;
        const isPinchHarmonic = opts?.hp || false;
        const isChord = opts?.chord || false;
        const bend = opts?.bn || 0;
        const slide = opts?.sl || -1;
        const hammerOn = opts?.ho || false;
        const pullOff = opts?.po || false;
        const tap = opts?.tp || false;
        const palmMute = opts?.pm || false;
        const tremolo = opts?.tr || false;
        const accent = opts?.ac || false;
        const sz = Math.max(12, 80 * scale * (H / 900));
        const half = sz / 2;
        const color = STRING_COLORS[string] || '#888';
        const dark = STRING_DIM[string] || '#222';

        if (sz < 6) {
            ctx.fillStyle = color;
            ctx.beginPath();
            ctx.arc(x, y, 2, 0, Math.PI * 2);
            ctx.fill();
            return;
        }

        // Open string: wide bar spanning the highway (only for standalone notes)
        if (fret === 0 && !isChord) {
            const hw = W * 0.26 * scale;
            const barH = Math.max(6, sz * 0.45);
            // Shadow
            ctx.fillStyle = dark;
            roundRect(ctx, W/2 - hw - 1, y - barH/2 - 1, hw * 2 + 2, barH + 2, 3);
            ctx.fill();
            // Body
            ctx.fillStyle = color;
            roundRect(ctx, W/2 - hw, y - barH/2, hw * 2, barH, 2);
            ctx.fill();
            // "0" label
            const fontSize = Math.max(8, sz * 0.5) | 0;
            ctx.fillStyle = '#fff';
            ctx.font = `bold ${fontSize}px sans-serif`;
            ctx.textAlign = 'center';
            ctx.textBaseline = 'middle';
            fillTextReadable('0', W/2, y);

            // Technique labels on open strings — PM, H/P/T, tremolo, and
            // accent markers are all meaningful on fret 0. Bend and slide
            // are omitted because they reference a fret position that the
            // centered bar doesn't visually convey. Matches the sz<14 gate
            // the fretted path uses so labels don't render on tiny bars.
            // Fixes #21.
            if (sz >= 14) {
                // H / P / T above
                if (hammerOn || pullOff || tap) {
                    const label = tap ? 'T' : (hammerOn ? 'H' : 'P');
                    ctx.fillStyle = '#fff';
                    ctx.font = `bold ${Math.max(9, sz * 0.3) | 0}px sans-serif`;
                    ctx.textAlign = 'center';
                    ctx.textBaseline = 'bottom';
                    fillTextReadable(label, W/2, y - barH/2 - 4);
                }
                // PM below
                if (palmMute) {
                    ctx.fillStyle = '#aaa';
                    ctx.font = `bold ${Math.max(8, sz * 0.25) | 0}px sans-serif`;
                    ctx.textAlign = 'center';
                    ctx.textBaseline = 'top';
                    fillTextReadable('PM', W/2, y + barH/2 + 2);
                }
                // Tremolo (wavy line above)
                if (tremolo) {
                    const ty = y - barH/2 - 6;
                    ctx.strokeStyle = '#ff0';
                    ctx.lineWidth = 1.5;
                    ctx.beginPath();
                    for (let i = -3; i <= 3; i++) {
                        const wx = W/2 + i * sz * 0.08;
                        const wy = ty + Math.sin(i * 2) * 3;
                        if (i === -3) ctx.moveTo(wx, wy);
                        else ctx.lineTo(wx, wy);
                    }
                    ctx.stroke();
                }
                // Accent caret above
                if (accent) {
                    const ay2 = y - barH/2 - 4;
                    ctx.strokeStyle = '#fff';
                    ctx.lineWidth = 2;
                    ctx.beginPath();
                    ctx.moveTo(W/2 - sz * 0.2, ay2 + 3);
                    ctx.lineTo(W/2, ay2 - 2);
                    ctx.lineTo(W/2 + sz * 0.2, ay2 + 3);
                    ctx.stroke();
                }
            }
            return;
        }

        if (isHarmonic) {
            // Diamond shape for harmonics
            const dh = half * 1.15;
            // Glow
            ctx.fillStyle = dark;
            ctx.beginPath();
            ctx.moveTo(x, y - dh - 3); ctx.lineTo(x + half + 3, y);
            ctx.lineTo(x, y + dh + 3); ctx.lineTo(x - half - 3, y);
            ctx.closePath(); ctx.fill();
            // Body
            ctx.fillStyle = color;
            ctx.beginPath();
            ctx.moveTo(x, y - dh); ctx.lineTo(x + half, y);
            ctx.lineTo(x, y + dh); ctx.lineTo(x - half, y);
            ctx.closePath(); ctx.fill();
            // Bright outline
            ctx.strokeStyle = STRING_BRIGHT[string] || '#fff';
            ctx.lineWidth = 2;
            ctx.beginPath();
            ctx.moveTo(x, y - dh); ctx.lineTo(x + half, y);
            ctx.lineTo(x, y + dh); ctx.lineTo(x - half, y);
            ctx.closePath(); ctx.stroke();
            // PH label for pinch harmonics
            if (isPinchHarmonic && sz >= 14) {
                ctx.fillStyle = '#ff0';
                ctx.font = `bold ${Math.max(8, sz * 0.25) | 0}px sans-serif`;
                ctx.textAlign = 'center';
                ctx.textBaseline = 'top';
                fillTextReadable('PH', x, y + dh + 2);
            }
        } else {
            // Glow
            ctx.fillStyle = dark;
            roundRect(ctx, x - half - 4, y - half - 4, sz + 8, sz + 8, sz / 3);
            ctx.fill();
            // Body
            ctx.fillStyle = color;
            roundRect(ctx, x - half, y - half, sz, sz, sz / 5);
            ctx.fill();
        }

        // Fret number
        const fontSize = Math.max(10, sz * 0.5) | 0;
        ctx.fillStyle = '#fff';
        ctx.font = `bold ${fontSize}px sans-serif`;
        ctx.textAlign = 'center';
        ctx.textBaseline = 'middle';
        fillTextReadable(String(fret), x, y);

        // Bend notation
        if (bend && bend > 0 && sz >= 12) {
            const lw = Math.max(2, sz / 10);
            const arrowH = sz * 0.55 * Math.min(bend, 2);  // taller for bigger bends
            const ay = y - half - 4;
            const tipY = ay - arrowH;

            ctx.strokeStyle = '#fff';
            ctx.lineWidth = lw;

            // Curved arrow
            ctx.beginPath();
            ctx.moveTo(x, ay);
            ctx.quadraticCurveTo(x + sz * 0.2, ay - arrowH * 0.5, x, tipY);
            ctx.stroke();

            // Arrowhead
            ctx.beginPath();
            ctx.moveTo(x - sz * 0.12, tipY + sz * 0.12);
            ctx.lineTo(x, tipY);
            ctx.lineTo(x + sz * 0.12, tipY + sz * 0.12);
            ctx.stroke();

            // Bend label: "full", "1/2", "1 1/2", "2"
            let label;
            if (bend === 0.5) label = '½';
            else if (bend === 1) label = 'full';
            else if (bend === 1.5) label = '1½';
            else if (bend === 2) label = '2';
            else label = bend.toFixed(1);

            ctx.fillStyle = '#fff';
            ctx.font = `bold ${Math.max(9, sz * 0.28) | 0}px sans-serif`;
            ctx.textAlign = 'center';
            ctx.textBaseline = 'bottom';
            fillTextReadable(label, x, tipY - 2);
        }

        if (sz < 14) return;  // Skip small technique labels

        // Slide indicator (diagonal arrow)
        if (slide >= 0) {
            const dir = slide > fret ? -1 : 1;  // arrow direction (up or down the neck); mirror handles lefty
            ctx.strokeStyle = '#fff';
            ctx.lineWidth = Math.max(2, sz / 10);
            ctx.beginPath();
            ctx.moveTo(x - sz * 0.3, y + dir * sz * 0.3);
            ctx.lineTo(x + sz * 0.3, y - dir * sz * 0.3);
            ctx.stroke();
            // Arrowhead
            ctx.beginPath();
            ctx.moveTo(x + sz * 0.3, y - dir * sz * 0.3);
            ctx.lineTo(x + sz * 0.15, y - dir * sz * 0.15);
            ctx.stroke();
        }

        // H/P/T label above note
        if (hammerOn || pullOff || tap) {
            const label = tap ? 'T' : (hammerOn ? 'H' : 'P');
            const ly = y - half - (bend > 0 ? sz * 0.6 : 4);
            ctx.fillStyle = '#fff';
            ctx.font = `bold ${Math.max(9, sz * 0.3) | 0}px sans-serif`;
            ctx.textAlign = 'center';
            ctx.textBaseline = 'bottom';
            fillTextReadable(label, x, ly);
        }

        // Palm mute (PM below note)
        if (palmMute) {
            ctx.fillStyle = '#aaa';
            ctx.font = `bold ${Math.max(8, sz * 0.25) | 0}px sans-serif`;
            ctx.textAlign = 'center';
            ctx.textBaseline = 'top';
            fillTextReadable('PM', x, y + half + 2);
        }

        // Tremolo (wavy line above)
        if (tremolo) {
            const ty = y - half - (bend > 0 ? sz * 0.7 : 6);
            ctx.strokeStyle = '#ff0';
            ctx.lineWidth = 1.5;
            ctx.beginPath();
            for (let i = -3; i <= 3; i++) {
                const wx = x + i * sz * 0.08;
                const wy = ty + Math.sin(i * 2) * 3;
                if (i === -3) ctx.moveTo(wx, wy);
                else ctx.lineTo(wx, wy);
            }
            ctx.stroke();
        }

        // Accent (> marker)
        if (accent) {
            const ay2 = y - half - 4;
            ctx.strokeStyle = '#fff';
            ctx.lineWidth = 2;
            ctx.beginPath();
            ctx.moveTo(x - sz * 0.2, ay2 + 3);
            ctx.lineTo(x, ay2 - 2);
            ctx.lineTo(x + sz * 0.2, ay2 + 3);
            ctx.stroke();
        }
    }

    function drawSustains(W, H) {
        // Same master-difficulty fallback as drawNotes/drawChords —
        // without this, sustain bars for filtered-out notes would
        // still render, leaving orphan rectangles where no note head
        // is drawn.
        const src = _filteredNotes !== null ? _filteredNotes : notes;
        for (const n of src) {
            if (n.sus <= 0.01) continue;
            const end = n.t + n.sus;
            if (end < currentTime || n.t > currentTime + VISIBLE_SECONDS) continue;

            const t0 = Math.max(n.t - currentTime, 0);
            const t1 = Math.min(end - currentTime, VISIBLE_SECONDS);
            if (t0 >= t1) continue;

            const p0 = project(t0), p1 = project(t1);
            if (!p0 || !p1) continue;

            const x0 = fretX(n.f, p0.scale, W);
            const x1 = fretX(n.f, p1.scale, W);
            const sw0 = Math.max(2, 6 * p0.scale);
            const sw1 = Math.max(2, 6 * p1.scale);

            ctx.fillStyle = STRING_DIM[n.s] || '#333';
            ctx.beginPath();
            ctx.moveTo(x0 - sw0, p0.y * H);
            ctx.lineTo(x0 + sw0, p0.y * H);
            ctx.lineTo(x1 + sw1, p1.y * H);
            ctx.lineTo(x1 - sw1, p1.y * H);
            ctx.fill();
        }
    }

    function drawNotes(W, H) {
        // Master-difficulty filter (slopsmith#48): when the source had
        // phrase-level ladder data, render from the mastery-filtered
        // array. _filteredNotes stays null for slider-disabled sources
        // so rendering falls through to the flat notes array unchanged.
        const src = _filteredNotes !== null ? _filteredNotes : notes;
        // Binary search for visible range
        const tMin = currentTime - 0.25;
        const tMax = currentTime + VISIBLE_SECONDS;
        let lo = bsearch(src, tMin);
        let hi = bsearch(src, tMax);

        // Include sustained notes
        while (lo > 0 && src[lo-1].t + src[lo-1].sus > currentTime) lo--;

        // Collect drawn positions for unison bend detection
        const drawnNotes = [];

        for (let i = hi - 1; i >= lo; i--) {
            const n = src[i];
            let tOff = n.t - currentTime;

            // Hold sustained notes at now line
            let p;
            if (tOff < -0.05 && n.sus > 0 && n.t + n.sus > currentTime) {
                p = { y: 0.82, scale: 1.0 };
            } else {
                p = project(tOff);
            }
            if (!p) continue;

            const x = fretX(n.f, p.scale, W);
            drawNote(W, H, x, p.y * H, p.scale, n.s, n.f, n);
            drawnNotes.push({ t: n.t, s: n.s, f: n.f, bn: n.bn || 0, x, y: p.y * H, scale: p.scale });
        }

        // Draw unison bend connectors
        drawUnisonBends(W, H, drawnNotes);
    }

    function drawUnisonBends(W, H, drawnNotes) {
        // Group notes by time (within 0.01s tolerance)
        const groups = [];
        const used = new Set();
        for (let i = 0; i < drawnNotes.length; i++) {
            if (used.has(i)) continue;
            const group = [drawnNotes[i]];
            used.add(i);
            for (let j = i + 1; j < drawnNotes.length; j++) {
                if (used.has(j)) continue;
                if (Math.abs(drawnNotes[j].t - drawnNotes[i].t) < 0.01) {
                    group.push(drawnNotes[j]);
                    used.add(j);
                }
            }
            if (group.length >= 2) groups.push(group);
        }

        for (const group of groups) {
            // Find pairs: one with bend, one without (or both with different bends)
            const bent = group.filter(n => n.bn > 0);
            const unbent = group.filter(n => n.bn === 0);
            if (bent.length === 0 || unbent.length === 0) continue;

            // Draw connector between each bent-unbent pair
            for (const bn of bent) {
                // Find the closest unbent note by string
                let closest = unbent[0];
                for (const ub of unbent) {
                    if (Math.abs(ub.s - bn.s) < Math.abs(closest.s - bn.s)) closest = ub;
                }

                const sz = Math.max(12, 80 * bn.scale * (H / 900));
                if (sz < 14) continue;

                // Draw a curved dashed line connecting bent note to target note
                const x1 = bn.x, y1 = bn.y;
                const x2 = closest.x, y2 = closest.y;
                const midX = (x1 + x2) / 2 + sz * 0.5;
                const midY = (y1 + y2) / 2;

                ctx.save();
                ctx.strokeStyle = '#60d0ff';
                ctx.lineWidth = Math.max(2, sz / 12);
                ctx.setLineDash([4, 4]);
                ctx.beginPath();
                ctx.moveTo(x1, y1);
                ctx.quadraticCurveTo(midX, midY, x2, y2);
                ctx.stroke();
                ctx.setLineDash([]);
                ctx.restore();

                // "U" label at midpoint
                const labelSz = Math.max(10, sz * 0.3) | 0;
                ctx.fillStyle = '#60d0ff';
                ctx.font = `bold ${labelSz}px sans-serif`;
                ctx.textAlign = 'center';
                ctx.textBaseline = 'middle';
                const cpX = (x1 + 2 * midX + x2) / 4;
                const cpY = (y1 + 2 * midY + y2) / 4;
                fillTextReadable('U', cpX + sz * 0.3, cpY);
            }
        }
    }

    function drawChords(W, H) {
        // See drawNotes — _filteredChords is null for slider-disabled
        // sources so we fall through to the flat chords array.
        const src = _filteredChords !== null ? _filteredChords : chords;
        _ensureChordRenderCache(src);

        const tMin = currentTime - 0.25;
        const tMax = currentTime + VISIBLE_SECONDS;
        const lo = bsearchChords(src, tMin);
        const hi = bsearchChords(src, tMax);

        _updateFretLinePreview(src, lo, hi);
        _drawFretLineChordPreview(W, H);

        for (let i = hi - 1; i >= lo; i--) {
            const ch = src[i];
            const p = project(ch.t - currentTime);
            if (!p) continue;

            const info = _chordRenderInfo.get(ch);
            const { isFull, baseFret } = info;

            const sorted = [...ch.notes].sort((a, b) => _inverted ? b.s - a.s : a.s - b.s);
            const sz = Math.max(10, 28 * p.scale * (H / 900));
            const spread = sz * 0.85;
            const minSpread = sz + 16 * p.scale;
            const actualSpread = Math.max(spread, minSpread);
            const actualTotalH = actualSpread * Math.max(0, sorted.length - 1);

            const { tmpl, getTemplateFret, isOpen } = getChordTemplateInfo(ch.id, chordTemplates);
            const nonZeroNotes = sorted.filter(cn => !isOpen(cn));
            const hasNonZero = nonZeroNotes.length >= 1;

            const frameLeftFret = baseFret;
            const frameRightFret = baseFret + CHORD_FRAME_FRETS;

            // Frame validation — log once per chord id rather than every frame.
            if (hasNonZero && !_frameMismatchWarned.has(ch.id)) {
                const notesInFrame = nonZeroNotes.every(cn => cn.f >= frameLeftFret && cn.f <= frameRightFret);
                if (!notesInFrame) {
                    _frameMismatchWarned.add(ch.id);
                    console.warn('Chord frame mismatch:', ch.id, { frameLeftFret, frameRightFret, nonZeroFrets: nonZeroNotes.map(cn => cn.f) });
                }
            }

            // X span between fretted notes (excluding open strings)
            const xMin = hasNonZero ? Math.min(...nonZeroNotes.map(cn => fretX(cn.f, p.scale, W))) : null;
            const xMax = hasNonZero ? Math.max(...nonZeroNotes.map(cn => fretX(cn.f, p.scale, W))) : null;

            // Muted chord (all notes muted): draw empty gray frame with X
            const allMuted = sorted.length > 0 && sorted.every(cn => cn.mt);
            if (allMuted) {
                const { boxX, boxW, boxTop, boxH } = _computeChordBox(p, H, W, sorted, sz, actualSpread, baseFret);

                ctx.strokeStyle = MUTE_BOX_STROKE;
                ctx.lineWidth = Math.max(2, sz / 6);
                roundRect(ctx, boxX, boxTop, boxW, boxH, 2);
                ctx.stroke();

                ctx.fillStyle = MUTE_BOX_BAR;
                ctx.fillRect(boxX, boxTop + 2, boxW, 4);

                // Gray X cross, centered in frame
                const xInset = sz * 0.6;
                const xStartX = boxX + xInset;
                const xEndX = boxX + boxW - xInset;
                ctx.beginPath();
                ctx.moveTo(xStartX, boxTop + sz * 0.5);
                ctx.lineTo(xEndX, boxTop + boxH - sz * 0.5);
                ctx.moveTo(xEndX, boxTop + sz * 0.5);
                ctx.lineTo(xStartX, boxTop + boxH - sz * 0.5);
                ctx.stroke();

                continue;
            }

            // Repeat chord (mid-chain): translucent box + bracket bar.
            if (!isFull) {
                const { boxX, boxW, boxTop, boxH } = _computeChordBox(p, H, W, sorted, sz, actualSpread, baseFret);

                ctx.fillStyle = REPEAT_BOX_FILL;
                roundRect(ctx, boxX, boxTop, boxW, boxH, 2);
                ctx.fill();

                ctx.fillStyle = REPEAT_BOX_BAR;
                ctx.fillRect(boxX, boxTop + 2, boxW, 4);

                continue;
            }

            // First-in-chain (or short chain): full chord rendering.
            // Bracket bar above the notes.
            if (hasNonZero || sorted.length >= 2) {
                const positions = (hasNonZero ? nonZeroNotes : sorted).map((cn, j) => ({
                    x: fretX(cn.f, p.scale, W),
                    y: p.y * H - actualTotalH / 2 + j * actualSpread,
                }));
                const barY = positions[0].y - sz * 0.7;
                const barLeft = hasNonZero ? xMin : fretX(frameLeftFret, p.scale, W);
                const barRight = hasNonZero ? xMax : fretX(frameRightFret, p.scale, W);

                ctx.fillStyle = REPEAT_BOX_BAR;
                ctx.lineWidth = Math.max(3, sz / 4);
                roundRect(ctx, barLeft - 2, barY - 2, barRight - barLeft + 4, 4, 2);
                ctx.fill();
                for (const pos of positions) {
                    ctx.fillRect(pos.x - 2, barY, 4, pos.y - sz / 2 - barY);
                }
            }

            // Chord name label
            if (!ch.hd && p.scale > 0.15 && tmpl && tmpl.name) {
                const labelY = hasNonZero
                    ? (p.y * H - actualTotalH / 2 - sz * 0.7 - sz * 0.4)
                    : (p.y * H - sz * 0.8);
                const labelX = hasNonZero
                    ? (xMin + xMax) / 2
                    : (sorted.length >= 2
                        ? (fretX(frameLeftFret, p.scale, W) + fretX(frameRightFret, p.scale, W)) / 2
                        : fretX(sorted[0].f, p.scale, W));
                ctx.fillStyle = '#fff';
                ctx.font = `bold ${Math.max(14, sz * 0.45) | 0}px sans-serif`;
                ctx.textAlign = 'center';
                ctx.textBaseline = 'bottom';
                fillTextReadable(tmpl.name, labelX, labelY);
            }

            // Notes — wide colored bar for open strings inside a chord,
            // normal note glyph otherwise.
            const chordPositions = [];
            const hasMultipleNotes = sorted.length >= 2;

            sorted.forEach((cn, j) => {
                const x = fretX(cn.f, p.scale, W);
                const ny = p.y * H - actualTotalH / 2 + j * actualSpread;

                // Open-string-in-chord wide bar — only when the note has no
                // technique flags. Otherwise fall back to drawNote so PM /
                // H / P / T / tremolo / accent labels still render (drawNote
                // is the only path that emits those labels).
                if (getTemplateFret(cn) === 0 && hasMultipleNotes && !_noteHasTechniqueFlags(cn)) {
                    const color = STRING_COLORS[cn.s] || '#888';
                    const dark = STRING_DIM[cn.s] || '#222';
                    const barH = sz;
                    const barLeft = fretX(frameLeftFret, p.scale, W);
                    const barRight = fretX(frameRightFret, p.scale, W);
                    ctx.fillStyle = dark;
                    roundRect(ctx, barLeft - 1, ny - barH / 2 - 1, barRight - barLeft + 2, barH + 2, 3);
                    ctx.fill();
                    ctx.fillStyle = color;
                    roundRect(ctx, barLeft, ny - barH / 2, barRight - barLeft, barH, 2);
                    ctx.fill();
                    const fontSize = Math.max(8, sz * 0.5) | 0;
                    ctx.fillStyle = '#fff';
                    ctx.font = `bold ${fontSize}px sans-serif`;
                    ctx.textAlign = 'center';
                    ctx.textBaseline = 'middle';
                    fillTextReadable('0', (barLeft + barRight) / 2, ny);
                } else {
                    drawNote(W, H, x, ny, p.scale, cn.s, cn.f, { ...cn, chord: true });
                }

                chordPositions.push({ s: cn.s, f: cn.f, bn: cn.bn || 0, x, y: ny, scale: p.scale });
            });

            // Unison bend within chord
            const bent = chordPositions.filter(n => n.bn > 0);
            const unbent = chordPositions.filter(n => n.bn === 0);
            if (bent.length > 0 && unbent.length > 0 && sz >= 14) {
                for (const bn of bent) {
                    let closest = unbent[0];
                    for (const ub of unbent) {
                        if (Math.abs(ub.s - bn.s) < Math.abs(closest.s - bn.s)) closest = ub;
                    }
                    const x1 = bn.x, y1 = bn.y;
                    const x2 = closest.x, y2 = closest.y;
                    const midX = (x1 + x2) / 2 + sz * 0.5;
                    const midY = (y1 + y2) / 2;

                    ctx.save();
                    ctx.strokeStyle = '#60d0ff';
                    ctx.lineWidth = Math.max(2, sz / 12);
                    ctx.setLineDash([4, 4]);
                    ctx.beginPath();
                    ctx.moveTo(x1, y1);
                    ctx.quadraticCurveTo(midX, midY, x2, y2);
                    ctx.stroke();
                    ctx.setLineDash([]);
                    ctx.restore();

                    const labelSz = Math.max(10, sz * 0.3) | 0;
                    ctx.fillStyle = '#60d0ff';
                    ctx.font = `bold ${labelSz}px sans-serif`;
                    ctx.textAlign = 'center';
                    ctx.textBaseline = 'middle';
                    const cpX = (x1 + 2 * midX + x2) / 4;
                    const cpY = (y1 + 2 * midY + y2) / 4;
                    fillTextReadable('U', cpX + sz * 0.3, cpY);
                }
            }
        }
    }

    function drawFretNumbers(W, H) {
        const y = H * 0.97;
        const pad = 3;
        const lo = 0;
        const hi = Math.ceil(displayMaxFret);
        const anchor = getAnchorAt(currentTime);

        ctx.font = 'bold 20px sans-serif';
        ctx.textAlign = 'center';
        ctx.textBaseline = 'middle';

        for (let fret = lo; fret <= hi; fret++) {
            if (fret < 0) continue;
            const x = fretX(fret, 1.0, W);
            const inAnchor = fret >= anchor.fret && fret <= anchor.fret + anchor.width;
            ctx.fillStyle = inAnchor ? '#e8c040' : '#8a6830';
            fillTextReadable(String(fret), x, y);
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────
    function drawLyrics(W, H) {
        if (!lyrics.length) return;

        const fontSize = Math.max(18, H * 0.028) | 0;
        const lineY = H * 0.04;

        // Rocksmith vocal markers: a trailing "-" means the syllable joins the
        // next one into a single word (no space); a trailing "+" marks the end
        // of an authored line. Build a flat list of authored lines so we can
        // cap rendering to a 2-line rolling window (current + upcoming).
        if (!lyrics._lines) {
            const lines = [];
            let line = null, word = null;

            const flushWord = () => {
                if (word && word.length) line.words.push(word);
                word = null;
            };
            const flushLine = () => {
                flushWord();
                if (line && line.words.length) lines.push(line);
                line = null;
            };

            for (let i = 0; i < lyrics.length; i++) {
                const l = lyrics[i];
                const raw = l.w || '';
                const endsLine = raw.endsWith('+');
                const continuesWord = raw.endsWith('-');

                // Safety fallback: if a song has no "+" markers at all, force a
                // line break on any gap > 4s so we never build a single giant line.
                if (line && i > 0) {
                    const prev = lyrics[i - 1];
                    if (l.t - (prev.t + prev.d) > 4.0) flushLine();
                }

                if (!line) line = { words: [], start: l.t, end: l.t + l.d };
                if (!word) word = [];

                word.push(l);
                line.end = Math.max(line.end, l.t + l.d);

                if (!continuesWord) flushWord();
                if (endsLine) flushLine();
            }
            flushLine();

            lyrics._lines = lines;
        }

        const allLines = lyrics._lines;
        if (!allLines.length) return;

        // Current line = most recently started line. Before the first line has
        // started, preview the first line if it's within 2s of starting.
        let currentIdx = -1;
        for (let i = 0; i < allLines.length; i++) {
            if (allLines[i].start <= currentTime) currentIdx = i;
            else break;
        }
        if (currentIdx === -1) {
            if (allLines[0].start - currentTime > 2.0) return;
            currentIdx = 0;
        }

        const currentLine = allLines[currentIdx];
        const nextLine = allLines[currentIdx + 1] || null;
        const gapToNext = nextLine ? (nextLine.start - currentLine.end) : Infinity;

        // Hide once the current line is clearly over and nothing relevant follows.
        if (currentTime > currentLine.end + 0.5 && gapToNext > 3.0) return;

        const linesToShow = [currentLine];
        if (nextLine && gapToNext <= 3.0) linesToShow.push(nextLine);

        const sylText = (s) => {
            const t = s.w || '';
            return (t.endsWith('+') || t.endsWith('-')) ? t.slice(0, -1) : t;
        };

        ctx.font = `bold ${fontSize}px sans-serif`;
        const spaceWidth = ctx.measureText(' ').width;
        const maxWidth = W * 0.8;

        // Respect authored line breaks; wrap only if a line overflows maxWidth.
        const rows = [];
        for (const authoredLine of linesToShow) {
            let row = [], rowWidth = 0;
            for (const wordSyls of authoredLine.words) {
                const parts = [];
                let wordWidth = 0;
                for (const s of wordSyls) {
                    const text = sylText(s);
                    const w = ctx.measureText(text).width;
                    parts.push({ syl: s, text, width: w });
                    wordWidth += w;
                }
                const advance = wordWidth + spaceWidth;
                if (row.length > 0 && rowWidth + advance > maxWidth) {
                    rows.push(row);
                    row = []; rowWidth = 0;
                }
                row.push({ parts, advance });
                rowWidth += advance;
            }
            if (row.length) rows.push(row);
        }

        const rowHeight = fontSize + 6;
        const totalHeight = rows.length * rowHeight + 10;
        let bgWidth = 0;
        for (const row of rows) {
            const rw = row.reduce((s, w) => s + w.advance, 0) - spaceWidth;
            if (rw > bgWidth) bgWidth = rw;
        }
        bgWidth = Math.min(bgWidth + 30, W * 0.85);

        ctx.fillStyle = 'rgba(0,0,0,0.7)';
        roundRect(ctx, W/2 - bgWidth/2, lineY - 4, bgWidth, totalHeight, 8);
        ctx.fill();

        ctx.textAlign = 'left';
        ctx.textBaseline = 'top';

        for (let r = 0; r < rows.length; r++) {
            const row = rows[r];
            const rowWidth = row.reduce((s, w) => s + w.advance, 0) - spaceWidth;
            let xPos = W/2 - rowWidth/2;
            const yPos = lineY + r * rowHeight + 2;

            for (const w of row) {
                for (const part of w.parts) {
                    const l = part.syl;
                    const isActive = currentTime >= l.t && currentTime < l.t + l.d;
                    const isPast = currentTime >= l.t + l.d;

                    if (isActive) {
                        ctx.fillStyle = '#4ae0ff';
                        ctx.font = `bold ${fontSize}px sans-serif`;
                    } else if (isPast) {
                        ctx.fillStyle = '#8899aa';
                        ctx.font = `normal ${fontSize}px sans-serif`;
                    } else {
                        ctx.fillStyle = '#556677';
                        ctx.font = `normal ${fontSize}px sans-serif`;
                    }

                    ctx.fillText(part.text, xPos, yPos);
                    xPos += part.width;
                }
                xPos += spaceWidth;
            }
        }
    }

    function roundRect(ctx, x, y, w, h, r) {
        ctx.beginPath();
        ctx.moveTo(x + r, y);
        ctx.lineTo(x + w - r, y);
        ctx.quadraticCurveTo(x + w, y, x + w, y + r);
        ctx.lineTo(x + w, y + h - r);
        ctx.quadraticCurveTo(x + w, y + h, x + w - r, y + h);
        ctx.lineTo(x + r, y + h);
        ctx.quadraticCurveTo(x, y + h, x, y + h - r);
        ctx.lineTo(x, y + r);
        ctx.quadraticCurveTo(x, y, x + r, y);
        ctx.closePath();
    }

    function bsearch(arr, time) {
        let lo = 0, hi = arr.length;
        while (lo < hi) {
            const mid = (lo + hi) >> 1;
            if (arr[mid].t < time) lo = mid + 1;
            else hi = mid;
        }
        return lo;
    }
    function bsearchChords(arr, time) {
        let lo = 0, hi = arr.length;
        while (lo < hi) {
            const mid = (lo + hi) >> 1;
            if (arr[mid].t < time) lo = mid + 1;
            else hi = mid;
        }
        return lo;
    }

    // ── Chord rendering — chains, frames, fretline preview (slopsmith#88) ──
    //
    // Rocksmith charts often repeat the same chord shape several times in a
    // row (e.g. a G strummed 4 times). We call a contiguous run of same-id
    // chords with gaps < CHAIN_GAP_THRESHOLD a "chain". Chains drive two
    // visual choices:
    //   • The first chord in a chain renders in full; subsequent chords in
    //     a chain of CHAIN_RENDER_FULL_MAX or longer render as a "repeat
    //     box" — a translucent boxed frame so the eye can see the rhythm
    //     pattern without re-scanning identical fret numbers.
    //   • Each chord anchors a CHORD_FRAME_FRETS-wide frame; muted and
    //     open-only chords inherit the frame from their predecessor so
    //     they don't snap to fret 0.
    //
    // We compute chain stats and frame anchors once per `src` array via
    // _ensureChordRenderCache (lazy, invalidates when the array reference
    // changes — which happens on chord ingest, mastery rebuild, or song
    // reset). The render path is then pure read.
    const CHAIN_GAP_THRESHOLD = 0.5;
    const CHAIN_RENDER_FULL_MAX = 4;
    const CHORD_FRAME_FRETS = 4;

    // Fretline preview: the static fret line at the bottom shows the chord
    // closest to the strum line (currentTime + FRETLINE_TARGET_OFFSET) within
    // the [target - FRETLINE_WINDOW_BEFORE, target + FRETLINE_WINDOW_AFTER]
    // window, as a teaching aid.
    const FRETLINE_TARGET_OFFSET = -0.25;
    const FRETLINE_WINDOW_BEFORE = 0.1;
    const FRETLINE_WINDOW_AFTER = 0.3;

    // Repeat / mute box colors.
    const REPEAT_BOX_FILL = 'rgba(48, 80, 128, 0.06)';
    const REPEAT_BOX_BAR = '#50a0dc';
    const MUTE_BOX_STROKE = '#6060809b';
    const MUTE_BOX_BAR = '#606080d1';

    // Reset all chord-render-derived state. Called from init() and
    // reconnect() so per-song state (preview, frame-mismatch warnings,
    // chain cache) doesn't leak across songs that reuse chord IDs.
    function _resetChordRenderState() {
        _lastChordOnFretLine = null;
        _chordFretLineNotes = [];
        _frameMismatchWarned.clear();
        _chordRenderCacheSrc = null;
        _chordRenderCacheInverted = null;
    }

    // True if a chord note carries per-strum technique data (bend,
    // hammer/pull/tap, slide, palm-mute, tremolo, accent, harmonic, pinch
    // harmonic, dead note). drawNote renders these as glyph labels —
    // alternate render paths (repeat box, open-string-in-chord wide bar)
    // bypass drawNote and so must fall back to the full path whenever a
    // technique flag is present, otherwise authored cues vanish silently.
    function _noteHasTechniqueFlags(n) {
        if (n.bn || n.ho || n.po || n.tp || n.pm || n.tr || n.ac || n.hm || n.hp || n.mt) return true;
        if (typeof n.sl === 'number' && n.sl >= 0) return true;
        return false;
    }
    function _chordHasTechniqueFlags(ch) {
        const notes = ch.notes;
        for (let i = 0; i < notes.length; i++) {
            if (_noteHasTechniqueFlags(notes[i])) return true;
        }
        return false;
    }

    // Template lookup: returns helpers that classify a chord note's fret
    // against its template. Open = template fret 0 (regardless of cn.f).
    function getChordTemplateInfo(chordId, chordTemplates) {
        const tmpl = chordTemplates[chordId];
        const tmplFrets = tmpl && tmpl.frets ? tmpl.frets : [];
        const getTemplateFret = (cn) => cn.s < tmplFrets.length ? tmplFrets[cn.s] : cn.f;
        const isOpen = (cn) => getTemplateFret(cn) === 0;
        return { tmpl, tmplFrets, getTemplateFret, isOpen };
    }

    // Build _chordRenderInfo for every chord in `src` if the cache is stale.
    // Two passes over the array: chain bounds, then base-fret resolution
    // (which can read previous chord's cached baseFret).
    function _ensureChordRenderCache(src) {
        if (_chordRenderCacheSrc === src && _chordRenderCacheInverted === _inverted) return;
        _chordRenderCacheSrc = src;
        _chordRenderCacheInverted = _inverted;

        // Pass 1: walk forward, marking chain index / length / isFull on a
        // per-chord WeakMap entry. A chain breaks when the next chord has a
        // different id OR the time gap is >= CHAIN_GAP_THRESHOLD.
        // Chords that carry per-strum technique flags (bend / palm-mute /
        // hammer / pull / tap / slide / tremolo / accent / harmonic / mute)
        // never collapse to a repeat box — those cues are authored on each
        // strum and must stay visible.
        let chainStart = 0;
        for (let i = 0; i <= src.length; i++) {
            const breakHere = (i === src.length) ||
                (i > chainStart && (src[i].id !== src[i - 1].id ||
                    Math.abs(src[i].t - src[i - 1].t) >= CHAIN_GAP_THRESHOLD));
            if (breakHere && i > chainStart) {
                const len = i - chainStart;
                for (let k = chainStart; k < i; k++) {
                    const chainIndex = k - chainStart;
                    const hasTechniques = _chordHasTechniqueFlags(src[k]);
                    _chordRenderInfo.set(src[k], {
                        chainIndex,
                        chainLen: len,
                        isFull: len < CHAIN_RENDER_FULL_MAX || chainIndex === 0 || hasTechniques,
                        baseFret: 0,  // filled in pass 2
                    });
                }
                chainStart = i;
            }
        }

        // Pass 2: resolve baseFret. Fretted chords use their own lowest
        // non-open fret; chained same-id chords inherit from the previous
        // entry; open-only / muted chords with a different-id predecessor
        // inherit that predecessor's frame too. The walk is forward so
        // prev's cached value is always present when we read it.
        for (let i = 0; i < src.length; i++) {
            const ch = src[i];
            const info = _chordRenderInfo.get(ch);
            const { isOpen } = getChordTemplateInfo(ch.id, chordTemplates);
            const sortedNotes = [...ch.notes].sort((a, b) => _inverted ? b.s - a.s : a.s - b.s);
            const nonZero = sortedNotes.filter(cn => !isOpen(cn));
            if (nonZero.length >= 1) {
                info.baseFret = Math.min(...nonZero.map(cn => cn.f));
            } else if (i > 0) {
                const prevInfo = _chordRenderInfo.get(src[i - 1]);
                info.baseFret = prevInfo ? prevInfo.baseFret : 0;
            } else {
                info.baseFret = 0;
            }
        }
    }

    // Compute the on-screen box for a chord (used by both muted and repeat
    // box renderings). Box height tracks the per-string note positions; box
    // width spans the CHORD_FRAME_FRETS frame anchored at info.baseFret.
    function _computeChordBox(p, H, W, sorted, sz, actualSpread, baseFret) {
        const actualTotalH = actualSpread * Math.max(0, sorted.length - 1);
        const yCenter = p.y * H;
        const boxTop = yCenter - actualTotalH / 2 - sz * 0.5;
        const boxBottom = boxTop + Math.max(sz, actualTotalH + sz);
        const boxX = fretX(baseFret, p.scale, W);
        const boxW = fretX(baseFret + CHORD_FRAME_FRETS, p.scale, W) - boxX;
        return { boxX, boxW, boxTop, boxH: boxBottom - boxTop };
    }

    // Search [lo, hi) for the chord we should preview on the static fret
    // line. Prefer the chord nearest the strum line that's within
    // [target - before, target + after]; if none match, fall back to the
    // first visible chord. Updates _lastChordOnFretLine / _chordFretLineNotes
    // only when the active chord changes (lets the preview persist while a
    // chord is held).
    function _updateFretLinePreview(src, lo, hi) {
        const targetTime = currentTime + FRETLINE_TARGET_OFFSET;
        let activeChord = null;
        let activeNotesOnFret = [];
        let bestChordTime = -Infinity;

        for (let i = lo; i < hi; i++) {
            const ch = src[i];
            if (ch.t >= targetTime - FRETLINE_WINDOW_BEFORE &&
                ch.t < targetTime + FRETLINE_WINDOW_AFTER &&
                ch.t > bestChordTime) {
                bestChordTime = ch.t;
                activeChord = ch;
                const { isOpen } = getChordTemplateInfo(ch.id, chordTemplates);
                const nonZero = ch.notes.filter(cn => !isOpen(cn));
                activeNotesOnFret = nonZero.length >= 1 ? nonZero.map(cn => ({ s: cn.s, f: cn.f })) : [];
            }
        }

        if (activeChord === null) {
            for (let i = lo; i < hi; i++) {
                const ch = src[i];
                const p = project(ch.t - currentTime);
                if (!p) continue;
                activeChord = ch;
                const { isOpen } = getChordTemplateInfo(ch.id, chordTemplates);
                const nonZero = ch.notes.filter(cn => !isOpen(cn));
                activeNotesOnFret = nonZero.length >= 1 ? nonZero.map(cn => ({ s: cn.s, f: cn.f })) : [];
                break;
            }
        }

        // Compare by chord OBJECT identity rather than .id — two strums of
        // the same chord template are different objects, so a chain like
        // (G normal) → (G all-muted) refreshes the preview instead of
        // leaving the first strum's fingerings stuck on the fret line.
        if (activeChord !== _lastChordOnFretLine) {
            _chordFretLineNotes = activeNotesOnFret;
            _lastChordOnFretLine = activeChord;
        }
    }

    function _drawFretLineChordPreview(W, H) {
        if (_chordFretLineNotes.length === 0) return;
        const strTop = H * 0.83;
        const strBot = H * 0.95;
        // Scale glyphs with H so preview stays proportionate at any
        // resolution / renderScale. Constants picked to match the prior
        // hardcoded 30px diameter / 24px font at H=900.
        const noteSize = Math.max(14, H * 0.033);
        const fontSize = Math.max(11, H * 0.027) | 0;
        ctx.font = `bold ${fontSize}px sans-serif`;
        ctx.textAlign = 'center';
        ctx.textBaseline = 'middle';
        for (const cn of _chordFretLineNotes) {
            const yi = _inverted ? 5 - cn.s : cn.s;
            const syl = strTop + (yi / 5) * (strBot - strTop);
            const fretXPos = fretX(cn.f, 1, W);
            ctx.fillStyle = STRING_COLORS[cn.s] || '#888';
            ctx.beginPath();
            ctx.arc(fretXPos, syl, noteSize / 2, 0, Math.PI * 2);
            ctx.fill();
            ctx.fillStyle = '#fff';
            fillTextReadable(String(cn.f), fretXPos, syl);
        }
    }

    // Rebuild the mastery-filtered note/chord arrays from _phrases +
    // _mastery. Called on `ready` and on every setMastery(). When
    // _phrases is null (slider-disabled source), we clear the filtered
    // arrays — drawNotes/drawChords fall through to the flat arrays.
    //
    // Output arrays are pre-sorted by time because phrase iterations
    // arrive in chronological order and within each level the notes/
    // chords are time-sorted already (PR 1's parser sorts them), so
    // concatenation preserves the order. No explicit sort needed.
    function _rebuildMasteryFilter() {
        // Null OR empty → fall through to flat arrays. The server's
        // chunked emission invariant means _phrases should never land
        // at `[]` in practice (it'd require the `phrases` message to
        // fire with zero data), but the defensive guard means a bug
        // on the way in wouldn't blank the chart.
        if (_phrases === null || _phrases.length === 0) {
            _filteredNotes = null;
            _filteredChords = null;
            _filteredAnchors = null;
            return;
        }
        const outNotes = [];
        const outChords = [];
        const outAnchors = [];
        for (const p of _phrases) {
            const n = p.levels.length;
            if (n === 0) continue;
            // Map slider fraction to a level index. `n` already equals
            // `max_difficulty + 1` for fully-authored phrases, and
            // equals the authored-level count otherwise — so indexing
            // into p.levels.length is both correct and defensive.
            const idx = Math.min(n - 1, Math.floor(_mastery * n));
            const lv = p.levels[idx];
            for (const x of lv.notes)   outNotes.push(x);
            for (const x of lv.chords)  outChords.push(x);
            // Anchors drive the fret zoom / pan. Keeping max-mastery
            // anchors while hiding higher-difficulty notes would leave
            // the highway panning into empty regions — filter them to
            // the same level as the notes they pair with.
            for (const x of lv.anchors) outAnchors.push(x);
        }
        _filteredNotes = outNotes;
        _filteredChords = outChords;
        _filteredAnchors = outAnchors;
    }

    // ── Public API ───────────────────────────────────────────────────────
    const api = {
        init(canvasEl, container) {
            canvas = canvasEl;
            _resizeContainer = container || null;
            // Size the canvas BEFORE installing the renderer so
            // _setRenderer's init/resize calls see the real dimensions
            // instead of the default 300x150 backing store. Otherwise
            // WebGL renderers would allocate framebuffers at the wrong
            // size and immediately have to tear them down when
            // api.resize fires afterwards.
            this.resize();
            // Install the default renderer on first init. If a caller
            // pre-selected a custom renderer before init ran (e.g.
            // app.js restoring a saved viz picker selection at page
            // load), re-apply that choice now that the canvas is
            // available instead of clobbering it with the default.
            // _setRenderer(_renderer) is correct: it re-applies the
            // selected renderer now that the canvas exists, and only
            // destroys the previous renderer if it had been
            // successfully init'd before this mount (so a pre-selected
            // renderer that never saw a canvas gets init'd fresh, not
            // destroy+init'd).
            _setRenderer(_renderer || _defaultRenderer);
            if (_resizeHandler) window.removeEventListener('resize', _resizeHandler);
            _resizeHandler = () => this.resize();
            window.addEventListener('resize', _resizeHandler);
            ready = false;
            notes = []; chords = []; beats = []; sections = []; anchors = []; chordTemplates = []; lyrics = []; toneChanges = []; toneBase = "";
            stringCount = 6;  // default until song_info arrives
            // Reset phrase ladder + filter (slopsmith#48). _mastery
            // persists across arrangement switches — the slider's
            // position stays put. Filter rebuilds on the next `ready`
            // once the new arrangement's phrases arrive (or stays
            // disabled if the new source has no phrase data).
            _phrases = null;
            _filteredNotes = null;
            _filteredChords = null;
            _filteredAnchors = null;
            _resetChordRenderState();
        },

        resize() {
            if (!canvas) return;
            let w, h;
            if (_resizeContainer) {
                const rect = _resizeContainer.getBoundingClientRect();
                w = rect.width;
                h = rect.height;
            } else {
                const controls = document.getElementById('player-controls');
                const controlsH = controls ? controls.offsetHeight : 50;
                w = document.documentElement.clientWidth;
                h = document.documentElement.clientHeight - controlsH;
            }
            canvas.style.width = w + 'px';
            canvas.style.height = h + 'px';
            canvas.width = Math.round(w * _renderScale);
            canvas.height = Math.round(h * _renderScale);
            // Notify the active renderer so WebGL / offscreen buffers
            // can recreate their framebuffers. Setting canvas.width
            // above already invalidates both 2D and WebGL state — any
            // renderer relying on persistent GPU resources listens here.
            //
            // Gated on _rendererInited: a renderer pre-selected via
            // setRenderer before api.init has run is stashed but not
            // initialized yet. Calling resize() on it would violate
            // the init-before-resize contract and can break renderers
            // that assume resize() means "canvas dims changed after
            // setup." The subsequent api.init will call its resize()
            // once init succeeds.
            if (_renderer && _rendererInited && typeof _renderer.resize === 'function') {
                try { _renderer.resize(canvas.width, canvas.height); }
                catch (e) { console.error('renderer resize:', e); }
            }
        },

        setRenderScale(scale) {
            _renderScale = Math.max(0.25, Math.min(1, scale));
            localStorage.setItem('renderScale', _renderScale);
            this.resize();
        },

        getRenderScale() { return _renderScale; },

        getInverted() { return _inverted; },
        setInverted(v) { _inverted = v; localStorage.setItem('invertHighway', v); },
        setLefty(on) {
            _lefty = !!on;
            localStorage.setItem('lefty', _lefty ? '1' : '0');
        },

        getLefty() { return _lefty; },

        // Master-difficulty (slopsmith#48). Per-instance: splitscreen
        // plugins that call createHighway() separately get their own
        // _mastery via closure.
        setMastery(fraction) {
            // Same NaN guard as the init (plugins could pass undefined
            // or a string that coerces badly). Silently ignore — the
            // caller probably meant to pass a number; keeping the
            // previous value is safer than propagating NaN into
            // Math.floor → p.levels[NaN].
            const next = Number(fraction);
            if (!Number.isFinite(next)) return;
            _mastery = Math.max(0, Math.min(1, next));
            _rebuildMasteryFilter();
        },
        getMastery() { return _mastery; },
        // Align with _rebuildMasteryFilter's own "null OR empty → fall
        // through" check. If we returned true for _phrases = [], the
        // slider would be enabled (via song:ready's hasPhraseData) but
        // dragging it would do nothing (filter stays null). Same
        // sentinel, same check, single source of truth.
        hasPhraseData() { return !!(_phrases && _phrases.length > 0); },

        connect(wsUrl, opts = {}) {
            _connectOpts = opts;
            ws = new WebSocket(wsUrl);
            ws.onclose = () => { console.log('WS closed'); };
            ws.onerror = (e) => { console.error('WS error', e); };
            ws.onmessage = (ev) => {
                const msg = JSON.parse(ev.data);
                if (msg.error) {
                    console.error('Server error:', msg.error);
                    if (opts.onError) opts.onError(msg.error);
                    else alert('Error: ' + msg.error);
                    return;
                }
                switch (msg.type) {
                    case 'loading':
                        console.log('Loading:', msg.stage);
                        break;
                    case 'song_info':
                        songInfo = msg;
                        // Pick up the active arrangement's string count.
                        // Prefer the explicit `stringCount` field (added
                        // in slopsmith-plugin-3dhighway#7); fall back to
                        // `tuning.length` for older servers that haven't
                        // started emitting it (works correctly for
                        // GP-imported sources where tuning is already
                        // truncated, and for sloppaks loaded against an
                        // updated lib/song.py); final fallback is 6 for
                        // safety so a missing/malformed payload doesn't
                        // surface as 0 strings.
                        //
                        // Clamp to [1, MAX_STRINGS] before storing —
                        // stringCount drives loop bounds in drawStrings
                        // and downstream plugins. A malformed payload
                        // (huge or zero / negative) would otherwise hang
                        // the UI or render no strings at all. 8 covers
                        // every real-world instrument we ship colors
                        // for; values above that fall back to '#888'
                        // anyway via the STRING_COLORS lookup so
                        // capping the loop bound costs nothing visible.
                        const MAX_STRINGS = 8;
                        let _sc;
                        if (typeof msg.stringCount === 'number' && msg.stringCount > 0) {
                            _sc = msg.stringCount;
                        } else if (Array.isArray(msg.tuning) && msg.tuning.length > 0) {
                            _sc = msg.tuning.length;
                        } else {
                            _sc = 6;
                        }
                        // Math.trunc(_sc) (with finite check) instead of
                        // `_sc | 0` — bitwise-OR forces 32-bit signed
                        // conversion, so any value ≥ 2^31 wraps negative
                        // and the Math.max(1, ...) clamp would land at
                        // 1 string. Math.trunc preserves the magnitude;
                        // the Math.min(MAX_STRINGS, ...) below caps it
                        // safely.
                        const _scTrunc = Number.isFinite(_sc) ? Math.trunc(_sc) : 1;
                        stringCount = Math.max(1, Math.min(MAX_STRINGS, _scTrunc));
                        if (opts.onSongInfo) {
                            opts.onSongInfo(msg);
                        } else {
                            document.getElementById('hud-artist').textContent = msg.artist;
                            document.getElementById('hud-title').textContent = msg.title;
                            document.getElementById('hud-arrangement').textContent = msg.arrangement;

                            // Clear any lingering audio-error banner from a prior song.
                            const existingAudioErr = document.getElementById('audio-error-banner');
                            if (existingAudioErr) existingAudioErr.remove();

                            // Server reported a concrete audio-pipeline failure and has
                            // no URL to give us — surface it instead of leaving the
                            // user with a cryptic "Empty src attribute" from audio.play().
                            if (!msg.audio_url && msg.audio_error) {
                                const banner = document.createElement('div');
                                banner.id = 'audio-error-banner';
                                banner.className = 'fixed top-4 left-1/2 -translate-x-1/2 z-[300] bg-red-900/95 border border-red-700 text-red-100 rounded-lg px-4 py-3 max-w-2xl shadow-xl';
                                banner.innerHTML = `
                                    <div class="flex items-start gap-3">
                                        <span class="text-xl leading-none">⚠</span>
                                        <div class="flex-1">
                                            <div class="font-semibold text-sm">Audio unavailable</div>
                                            <div class="text-xs text-red-200 mt-1"></div>
                                        </div>
                                        <button class="text-red-300 hover:text-white text-lg leading-none" aria-label="Dismiss">✕</button>
                                    </div>`;
                                banner.querySelector('.text-xs').textContent = msg.audio_error;
                                banner.querySelector('button').addEventListener('click', () => banner.remove());
                                document.body.appendChild(banner);
                            }

                            if (msg.audio_url) {
                                const audio = document.getElementById('audio');
                                if (!audio.src || !audio.src.includes(msg.audio_url.split('/').pop())) {
                                    audio.src = msg.audio_url;
                                    audio.load();

                                    // Show buffering overlay
                                    let overlay = document.getElementById('audio-buffer-overlay');
                                    if (!overlay) {
                                        overlay = document.createElement('div');
                                        overlay.id = 'audio-buffer-overlay';
                                        overlay.className = 'fixed inset-0 z-[200] flex items-center justify-center bg-black/60 backdrop-blur-sm';
                                        overlay.innerHTML = `
                                            <div class="bg-dark-700 border border-gray-700 rounded-2xl p-6 w-72 text-center shadow-2xl">
                                                <div class="text-sm text-gray-300 mb-3">Loading audio...</div>
                                                <div style="height:6px;background:#1a1a2e;border-radius:999px;overflow:hidden">
                                                    <div id="audio-buffer-bar" style="height:100%;background:linear-gradient(90deg,#4080e0,#60a0ff);border-radius:999px;width:0%;transition:width 0.3s"></div>
                                                </div>
                                                <div class="text-xs text-gray-500 mt-2" id="audio-buffer-pct">0%</div>
                                            </div>`;
                                        document.body.appendChild(overlay);
                                    }

                                    const bar = document.getElementById('audio-buffer-bar');
                                    const pct = document.getElementById('audio-buffer-pct');

                                    const MIN_BUFFER_SECS = 30;

                                    function onProgress() {
                                        if (audio.buffered.length > 0 && audio.duration > 0) {
                                            const loaded = audio.buffered.end(audio.buffered.length - 1);
                                            const p = Math.round((loaded / audio.duration) * 100);
                                            if (bar) bar.style.width = p + '%';
                                            if (pct) pct.textContent = p + '%';
                                            // Dismiss when enough is buffered
                                            if (loaded >= MIN_BUFFER_SECS || loaded >= audio.duration) {
                                                cleanup();
                                            }
                                        }
                                    }

                                    function cleanup() {
                                        audio.removeEventListener('progress', onProgress);
                                        audio.removeEventListener('canplaythrough', cleanup);
                                        const ol = document.getElementById('audio-buffer-overlay');
                                        if (ol) ol.remove();
                                    }

                                    audio.addEventListener('progress', onProgress);
                                    // Fallback: also dismiss on canplaythrough
                                    audio.addEventListener('canplaythrough', cleanup, { once: true });
                                }
                            }
                            // Populate arrangement dropdown
                            if (msg.arrangements) {
                                const sel = document.getElementById('arr-select');
                                sel.innerHTML = msg.arrangements.map(a =>
                                    `<option value="${a.index}" ${a.index === msg.arrangement_index ? 'selected' : ''}>${a.name} (${a.notes})</option>`
                                ).join('');
                            }
                        }
                        // Plugin context API — broadcast current song state
                        if (window.slopsmith) {
                            const wsPath = ws.url.split('/ws/highway/')[1] || '';
                            const filename = decodeURIComponent(wsPath.split('?')[0]);
                            window.slopsmith.currentSong = {
                                filename,
                                title: msg.title,
                                artist: msg.artist,
                                duration: msg.duration,
                                arrangement: msg.arrangement,
                                arrangementIndex: msg.arrangement_index,
                                arrangements: msg.arrangements || [],
                                tuning: msg.tuning,
                                capo: msg.capo,
                                format: msg.format,
                            };
                            window.slopsmith.emit('song:loaded', window.slopsmith.currentSong);
                        }
                        break;
                    case 'beats': beats = msg.data; break;
                    case 'sections': sections = msg.data; break;
                    case 'anchors':
                        anchors = msg.data;
                        if (anchors.length) {
                            displayMaxFret = Math.max(anchors[0].fret + anchors[0].width + 3, 8);
                        }
                        break;
                    case 'chord_templates': chordTemplates = msg.data; break;
                    case 'lyrics': lyrics = msg.data; break;
                    case 'tone_changes': toneChanges = msg.data; toneBase = msg.base || ""; break;
                    case 'notes': notes = notes.concat(msg.data); break;
                    case 'chords': chords = chords.concat(msg.data); break;
                    case 'phrases':
                        // Accumulate chunks but DON'T rebuild the filter
                        // until `ready` — rebuilding per chunk would
                        // cause visual flicker (partial filtered array
                        // visible while later chunks are still arriving)
                        // and duplicate work.
                        if (_phrases === null) _phrases = [];
                        for (const p of msg.data) _phrases.push(p);
                        break;
                    case 'ready':
                        ready = true;
                        _rebuildMasteryFilter();
                        console.log(`Highway ready: ${notes.length} notes, ${chords.length} chords` +
                            (_phrases !== null ? `, ${_phrases.length} phrases (mastery ${Math.round(_mastery * 100)}%)` : ""));
                        if (!animFrame) draw();
                        if (api._onReady) api._onReady();
                        // Broadcast to interested listeners (e.g. the
                        // difficulty-slider disabled-state update in
                        // app.js). Fires on every `ready`, including
                        // arrangement switches — unlike `_onReady`,
                        // which is a single-use callback slot.
                        if (window.slopsmith) {
                            // Reuse api.hasPhraseData so the emit and
                            // the public getter agree on the sentinel.
                            window.slopsmith.emit('song:ready', {
                                hasPhraseData: api.hasPhraseData(),
                            });
                        }
                        break;
                }
            };
        },

        setTime(t) { chartTime = t; currentTime = t + avOffsetSec; },
        setAvOffset(ms) { avOffsetSec = (Number(ms) || 0) / 1000; currentTime = chartTime + avOffsetSec; },
        getAvOffset() { return avOffsetSec * 1000; },

        getBPM(t) {
            // Calculate BPM from beat intervals near time t
            if (beats.length < 2) return 120;
            let closest = 0;
            for (let i = 1; i < beats.length; i++) {
                if (Math.abs(beats[i].time - t) < Math.abs(beats[closest].time - t)) closest = i;
            }
            // Average interval from nearby beats
            const start = Math.max(0, closest - 2);
            const end = Math.min(beats.length - 1, closest + 2);
            let sum = 0, count = 0;
            for (let i = start; i < end; i++) {
                sum += beats[i + 1].time - beats[i].time;
                count++;
            }
            return count > 0 ? 60 / (sum / count) : 120;
        },

        getBeats() { return beats; },
        getTime() { return chartTime; },
        getNotes() { return notes; },
        getChords() { return chords; },
        // Live reference to the chord-template lookup table —
        // `getChords()[i].id` is an index into this array. Each
        // template carries `{ name, fingers, frets }`:
        //   - name:    chord name string ("Em", "Cmaj7", …)
        //   - fingers: per-string finger numbers (length matches
        //              the tuning's string count; -1 = unused, 0 =
        //              open string, n > 0 = finger number). RS XML
        //              sources populate real values; GP imports
        //              currently emit all -1.
        //   - frets:   per-string fret numbers, same indexing.
        // Read-only: overlay plugins should NOT mutate the array or
        // its entries. Not difficulty-filter-aware (templates are
        // static metadata; every chord_id referenced by `getChords()`
        // is guaranteed valid).
        getChordTemplates() { return chordTemplates; },
        getToneChanges() { return toneChanges; },
        getToneBase() { return toneBase; },
        getSections() { return sections; },
        getSongInfo() { return songInfo; },
        // Number of strings on the active arrangement
        // (slopsmith-plugin-3dhighway#7). 4 for bass, 6 for guitar,
        // 7+ for extended-range GP imports. Plugins should size
        // string-indexed UI / geometry against THIS rather than
        // assuming 6. Defaults to 6 between songs (until the next
        // song_info message arrives).
        getStringCount() { return stringCount; },
        addDrawHook(fn) { _drawHooks.push(fn); },
        removeDrawHook(fn) { _drawHooks = _drawHooks.filter(h => h !== fn); },
        project(tOffset) { return project(tOffset); },
        fretX(fret, scale, w) { return fretX(fret, scale, w); },

        /** Use when drawing text inside the lefty mirror; noop when not lefty. */
        fillTextUnmirrored(text, x, y) { fillTextReadable(text, x, y); },

        toggleLyrics() {
            showLyrics = !showLyrics;
            localStorage.setItem('showLyrics', String(showLyrics));
            if (_onLyricsChange) _onLyricsChange(showLyrics);
        },

        getLyricsVisible() { return showLyrics; },
        setLyricsVisible(v) {
            showLyrics = !!v;
            if (_onLyricsChange) _onLyricsChange(showLyrics);
        },
        setOnLyricsChange(fn) { _onLyricsChange = fn; },

        reconnect(filename, arrangement) {
            // Close old WS but keep audio + animation running
            if (ws) { ws.close(); ws = null; }
            ready = false;
            notes = []; chords = []; beats = []; sections = []; anchors = []; chordTemplates = []; lyrics = []; toneChanges = []; toneBase = "";
            stringCount = 6;  // default until song_info arrives
            // Reset phrase ladder + filter (slopsmith#48). _mastery
            // persists across arrangement switches — the slider's
            // position stays put. Filter rebuilds on the next `ready`
            // once the new arrangement's phrases arrive (or stays
            // disabled if the new source has no phrase data).
            _phrases = null;
            _filteredNotes = null;
            _filteredChords = null;
            _filteredAnchors = null;
            _resetChordRenderState();
            const arrParam = arrangement !== undefined ? `?arrangement=${arrangement}` : '';
            // filename might already be encoded from data-play attribute
            const decoded = decodeURIComponent(filename);
            const wsUrl = `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}/ws/highway/${decoded}${arrParam}`;
            console.log('reconnect:', wsUrl);
            this.connect(wsUrl, _connectOpts);
        },

        stop() {
            if (animFrame) { cancelAnimationFrame(animFrame); animFrame = null; }
            if (ws) { ws.close(); ws = null; }
            if (_resizeHandler) {
                window.removeEventListener('resize', _resizeHandler);
                _resizeHandler = null;
            }
            // Release the renderer's GPU / DOM / event-listener resources
            // when leaving the player — anything it allocated in init()
            // should be torn down here so navigating away doesn't leak.
            // Crucially we KEEP `_renderer` (the instance/selection) so
            // that the next api.init() can re-apply the same visualization
            // on the new canvas. _rendererInited flips to false so
            // _setRenderer knows not to call destroy() again on this
            // already-destroyed instance.
            _destroyCurrentIfInited();
            ready = false;
        },

        /**
         * Install a custom renderer. Contract (slopsmith#36):
         *   r.init(canvas, bundle) — one-time setup; owns getContext().
         *   r.draw(bundle)         — per rAF frame.
         *   r.resize(w, h)         — optional; called when canvas dims change.
         *   r.destroy()            — optional; release resources.
         * Pass null or undefined to restore the default renderer.
         *
         * Custom renderers receive a data bundle (see _makeBundle) that
         * already applies the master-difficulty filter — the notes /
         * chords / anchors arrays are the right set to render regardless
         * of slider position. Use _drawHooks only for the default
         * renderer; they're a 2D-only contract.
         */
        setRenderer(r) { _setRenderer(r); },
    };
    return api;
}
const highway = createHighway();
window.highway = highway; // expose for plugins
highway.setOnLyricsChange(function(visible) {
    const btn = document.getElementById('btn-lyrics');
    if (btn) {
        btn.textContent = visible ? 'Lyrics \u2713' : 'Lyrics \u2717';
        btn.className = visible
            ? 'px-3 py-1.5 bg-purple-900/40 hover:bg-purple-900/60 rounded-lg text-xs text-purple-300 transition'
            : 'px-3 py-1.5 bg-dark-600 hover:bg-dark-500 rounded-lg text-xs text-gray-500 transition';
    }
});
