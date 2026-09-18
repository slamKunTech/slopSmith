/* MIDI Converter screen: folder picking, job control, progress polling.
   Talks to /api/convert/* (convert_api.py on the server). */
window.convertUI = (() => {
    let jobId = null;
    let pollTimer = null;
    let logOffset = 0;

    const $ = id => document.getElementById(id);

    function setMsg(text, cls) {
        const el = $('cv-msg');
        el.textContent = text || '';
        el.className = 'self-center text-sm ' + (cls || 'text-gray-400');
    }

    /* ── folder picking: Electron native dialog, fallback web browser modal ── */
    async function pickDir(inputId) {
        if (window.slopsmithDesktop?.pickDirectory) {
            const p = await window.slopsmithDesktop.pickDirectory();
            if (p) $(inputId).value = p;
            return;
        }
        openBrowserModal(inputId);
    }

    let bmTarget = null, bmPath = null;

    function openBrowserModal(inputId) {
        bmTarget = inputId;
        let modal = $('cv-browse-modal');
        if (!modal) {
            modal = document.createElement('div');
            modal.id = 'cv-browse-modal';
            modal.className = 'fixed inset-0 z-[100] bg-black/70 flex items-center justify-center p-6';
            modal.innerHTML = `
              <div class="bg-dark-800 border border-gray-700 rounded-2xl w-full max-w-lg max-h-[70vh] flex flex-col">
                <div class="px-5 py-4 border-b border-gray-800 flex items-center gap-3">
                  <button onclick="convertUI.bmUp()" class="text-gray-400 hover:text-white">↑</button>
                  <div id="cv-bm-path" class="text-xs text-gray-500 font-mono truncate flex-1"></div>
                </div>
                <div id="cv-bm-list" class="overflow-y-auto flex-1 p-2 space-y-1"></div>
                <div class="px-5 py-4 border-t border-gray-800 flex justify-end gap-3">
                  <button onclick="convertUI.bmClose()" class="text-sm text-gray-400 hover:text-white px-4 py-2">Cancel</button>
                  <button onclick="convertUI.bmChoose()" class="text-sm bg-accent hover:bg-accent-light text-white px-4 py-2 rounded-lg">Choose this folder</button>
                </div>
              </div>`;
            document.body.appendChild(modal);
        }
        modal.classList.remove('hidden');
        bmLoad(bmPath || (window.homeDir || '/'));
    }

    async function bmLoad(path) {
        const r = await fetch('/api/convert/browse?path=' + encodeURIComponent(path));
        if (!r.ok) { setMsg('browse failed: ' + (await r.text()), 'text-red-400'); return; }
        const data = await r.json();
        bmPath = data.path;
        $('cv-bm-path').textContent = data.path;
        const list = $('cv-bm-list');
        list.innerHTML = '';
        for (const d of data.dirs) {
            const row = document.createElement('button');
            row.className = 'w-full text-left px-3 py-2 rounded-lg hover:bg-dark-600 text-sm text-gray-300 flex justify-between';
            row.innerHTML = `<span class="truncate">📁 ${d.name}</span>` +
                (d.midi_count ? `<span class="text-xs text-gray-600 shrink-0 ml-2">${d.midi_count} mid</span>` : '');
            row.ondblclick = () => bmLoad(d.path);
            row.onclick = () => bmLoad(d.path);
            list.appendChild(row);
        }
    }

    function bmUp() { if (bmPath && bmPath !== '/') bmLoad(bmPath.replace(/\/[^/]*\/?$/, '') || '/'); }
    function bmClose() { $('cv-browse-modal')?.classList.add('hidden'); }
    function bmChoose() {
        if (bmTarget && bmPath) $(bmTarget).value = bmPath;
        bmClose();
    }

    /* ── job control ─────────────────────────────────────────────────────── */
    async function start() {
        const stageA = $('cv-stage-a').checked, stageB = $('cv-stage-b').checked;
        if (!stageA && !stageB) { setMsg('pick at least one stage', 'text-red-400'); return; }
        const body = {
            midi_src: $('cv-midi-src').value.trim() || null,
            gp5_dir: $('cv-gp5-dir').value.trim(),
            sloppak_out: $('cv-sloppak-out').value.trim() || null,
            skip_midi: !stageA,
            skip_sloppak: !stageB,
            overwrite_midi: $('cv-overwrite').checked,
            force_sloppak: $('cv-force').checked,
            jobs: parseInt($('cv-jobs').value || '6', 10),
            sloppak_workers: parseInt($('cv-sworkers').value || '4', 10),
        };
        if (!body.gp5_dir) { setMsg('GP5 output folder is required', 'text-red-400'); return; }
        setMsg('starting…');
        let r;
        try {
            r = await fetch('/api/convert/start', {
                method: 'POST', headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify(body),
            });
        } catch (e) { setMsg('server unreachable', 'text-red-400'); return; }
        if (!r.ok) { setMsg((await r.json()).detail || r.statusText, 'text-red-400'); return; }
        const { job_id } = await r.json();
        jobId = job_id; logOffset = 0;
        $('cv-jobid').textContent = '#' + job_id;
        $('cv-progress').classList.remove('hidden');
        $('cv-log').textContent = '';
        $('cv-start').classList.add('hidden');
        $('cv-cancel').classList.remove('hidden');
        setMsg('');
        poll(true);
    }

    async function cancel() {
        if (!jobId) return;
        await fetch(`/api/convert/job/${jobId}/cancel`, { method: 'POST' });
        setMsg('cancelling…');
    }

    async function poll(reset) {
        if (!jobId) return;
        clearTimeout(pollTimer);
        try {
            const r = await fetch(`/api/convert/job/${jobId}?log_offset=${reset ? 0 : logOffset}`);
            if (!r.ok) return;
            const j = await r.json();
            render(j, reset);
            if (['running', 'starting'].includes(j.state)) pollTimer = setTimeout(poll, 1500);
            else {
                $('cv-start').classList.remove('hidden');
                $('cv-cancel').classList.add('hidden');
                const ok = j.state === 'done';
                setMsg(ok ? 'finished' : j.state, ok ? 'text-green-400' : 'text-red-400');
            }
        } catch (e) { /* server restart etc. */ }
    }

    function render(j, reset) {
        $('cv-state').textContent = j.state;
        $('cv-stage-label').textContent = j.stage || '';
        const a = j.stage_a, b = j.stage_b;
        const skipA = j.args.skip_midi, skipB = j.args.skip_sloppak;
        setBar('cv-a', a, skipA);
        setBar('cv-b', b, skipB);
        $('cv-ok').textContent = a.ok + b.ok;
        $('cv-fail').textContent = a.fail + b.fail;
        $('cv-skip').textContent = a.skip + b.skip;
        if (reset) logOffset = 0;
        const logEl = $('cv-log');
        if (j.log.length) {
            if (reset) logEl.textContent = '';
            logEl.textContent += j.log.join('\n') + '\n';
            logEl.scrollTop = logEl.scrollHeight;
        }
        logOffset = j.log_len;
    }

    function setBar(prefix, c, disabled) {
        const wrap = $(prefix + '-bar').parentElement.parentElement;
        wrap.classList.toggle('opacity-30', !!disabled);
        const total = c.total || 0;
        $(prefix + '-bar').style.width = total ? Math.min(100, c.done / total * 100) + '%' : '0%';
        $(prefix + '-count').textContent = total ? `${c.done}/${total}` : '';
    }

    function onShow() {
        // resume an existing job if one is still running
        pollTimer && clearTimeout(pollTimer);
        fetch('/api/convert/jobs').then(r => r.json()).then(jobs => {
            const active = jobs.find(x => ['running', 'starting'].includes(x.state));
            const last = jobs[0];
            const j = active || last;
            if (!j) return;
            jobId = j.id;
            $('cv-jobid').textContent = '#' + j.id;
            $('cv-progress').classList.remove('hidden');
            if (active) {
                logOffset = 0;
                $('cv-start').classList.add('hidden');
                $('cv-cancel').classList.remove('hidden');
                poll(true);
            } else {
                render(j, true);
            }
        }).catch(() => {});
    }

    return { pickDir, start, cancel, onShow, bmUp, bmClose, bmChoose };
})();
