"""Web API for the midi -> gp5 -> sloppak pipeline.

Mounts onto the slopsmith FastAPI app (server.py include_router). Wraps the
midi2sloppak.py orchestrator (slopsmith-desktop/src/midi2gp5/) as background
jobs with progress parsed from its stdout, plus a read-only directory browser
for UIs that run without the Electron folder picker.
"""
import os
import re
import signal
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path

from fastapi import APIRouter, HTTPException
from pydantic import BaseModel

router = APIRouter(prefix="/api/convert", tags=["convert"])

# ── Pipeline config ───────────────────────────────────────────────────────────
DESKTOP_SRC = Path("/Users/mac/codes/slopSmith/slopsmith-desktop/src/midi2gp5")
ORCHESTRATOR = DESKTOP_SRC / "midi2sloppak.py"
# The orchestrator itself is stdlib-only; run it with this server's interpreter.
PIPELINE_PY = sys.executable

MAX_RUNNING_JOBS = 1
LOG_CAP = 400  # keep last N lines per job

STAGE_A_RE = re.compile(r"batch_convert\.py")
STAGE_B_RE = re.compile(r"GuitarProFiles2sloppak_convertor\.py")
A_FOUND_RE = re.compile(r"Found (\d+) MIDI")
A_PROGRESS_RE = re.compile(r"progress (\d+)/(\d+) ok=(\d+) fail=(\d+) skip=(\d+)")
A_DONE_RE = re.compile(r"^DONE ok=(\d+) fail=(\d+) skip=(\d+)")
B_TOTAL_RE = re.compile(r"GP files: (\d+)\s+already converted: (\d+)\s+to convert: (\d+)")
B_DONE_RE = re.compile(r"^Done: (\d+)/(\d+) converted\.")


class Job:
    def __init__(self, job_id: str, args: dict, cmd: list[str]):
        self.id = job_id
        self.args = args
        self.cmd = cmd
        self.state = "starting"          # starting|running|done|failed|cancelled
        self.stage = ""                  # current sub-stage label
        self.created = time.time()
        self.ended: float | None = None
        self.proc: subprocess.Popen | None = None
        self.log: list[str] = []
        self.cancelled = False
        # stage A counters (MIDI -> GP5)
        self.a_total = self.a_done = self.a_ok = self.a_fail = self.a_skip = 0
        # stage B counters (GP5 -> sloppak)
        self.b_total = self.b_done = self.b_ok = self.b_fail = self.b_skip = 0
        self._lock = threading.Lock()

    def append(self, line: str):
        with self._lock:
            self.log.append(line)
            if len(self.log) > LOG_CAP:
                del self.log[: len(self.log) - LOG_CAP]

    def snapshot(self, log_offset: int = 0) -> dict:
        with self._lock:
            return {
                "id": self.id,
                "state": self.state,
                "stage": self.stage,
                "created": self.created,
                "ended": self.ended,
                "args": self.args,
                "stage_a": {"total": self.a_total, "done": self.a_done,
                            "ok": self.a_ok, "fail": self.a_fail, "skip": self.a_skip},
                "stage_b": {"total": self.b_total, "done": self.b_done,
                            "ok": self.b_ok, "fail": self.b_fail, "skip": self.b_skip},
                "log": self.log[log_offset:],
                "log_len": len(self.log),
            }

    def parse_line(self, line: str):
        if line.startswith("$ "):
            if STAGE_A_RE.search(line):
                self.stage = "Stage A · MIDI → GP5"
            elif STAGE_B_RE.search(line):
                self.stage = "Stage B · GP5 → sloppak"
            return
        m = A_FOUND_RE.search(line)
        if m:
            self.a_total = int(m.group(1))
            return
        m = A_PROGRESS_RE.search(line)
        if m:
            self.a_done, self.a_total, self.a_ok = (int(m.group(1)), int(m.group(2)),
                                                    int(m.group(3)))
            self.a_fail, self.a_skip = int(m.group(4)), int(m.group(5))
            return
        m = A_DONE_RE.search(line)
        if m:
            self.a_done = self.a_ok + self.a_fail + self.a_skip
            return
        m = B_TOTAL_RE.search(line)
        if m:
            self.b_total = int(m.group(1))
            self.b_skip = int(m.group(2))
            return
        m = B_DONE_RE.search(line)
        if m:
            self.b_done = int(m.group(1))
            return
        if line.strip().startswith("✓"):
            self.b_ok += 1
        elif line.strip().startswith("✗") or line.startswith("[FAIL]"):
            self.b_fail += 1


_JOBS: dict[str, Job] = {}
_JOBS_LOCK = threading.Lock()


def _reader_loop(job: Job):
    """Pump orchestrator stdout into the job until the process exits."""
    try:
        assert job.proc is not None and job.proc.stdout is not None
        for raw in iter(job.proc.stdout.readline, ""):
            line = raw.rstrip("\n")
            if not line.strip():
                continue
            job.append(line)
            with job._lock:
                job.parse_line(line)
        rc = job.proc.wait()
        with job._lock:
            if job.cancelled:
                job.state = "cancelled"
            elif rc == 0:
                job.state = "done"
                job.stage = "finished"
            else:
                job.state = "failed"
            job.ended = time.time()
    except Exception as e:  # reader must never crash the server
        job.append(f"[convert_api] reader error: {e}")
        with job._lock:
            job.state = "failed"
            job.ended = time.time()


class StartRequest(BaseModel):
    midi_src: str | None = None
    gp5_dir: str
    sloppak_out: str | None = None
    skip_midi: bool = False
    skip_sloppak: bool = False
    overwrite_midi: bool = False
    force_sloppak: bool = False
    jobs: int = 6
    sloppak_workers: int = 4
    # advanced overrides (leave empty to use midi2sloppak.py defaults)
    midi_python: str | None = None
    sloppak_python: str | None = None
    orchestrator: str | None = None


@router.post("/start")
def start(req: StartRequest):
    orch = Path(req.orchestrator or ORCHESTRATOR)
    if not orch.is_file():
        raise HTTPException(400, f"orchestrator not found: {orch}")

    gp5_dir = Path(req.gp5_dir).expanduser()
    if not gp5_dir.is_absolute():
        raise HTTPException(400, "gp5_dir must be an absolute path")
    if req.skip_midi and not gp5_dir.is_dir():
        raise HTTPException(400, f"--skip-midi requires an existing gp5_dir: {gp5_dir}")
    if not req.skip_midi:
        if not req.midi_src:
            raise HTTPException(400, "midi_src is required unless skip_midi")
        src = Path(req.midi_src).expanduser()
        if not src.is_dir():
            raise HTTPException(400, f"midi_src is not a directory: {src}")
    if not req.skip_sloppak:
        if not req.sloppak_out:
            raise HTTPException(400, "sloppak_out is required unless skip_sloppak")
        out = Path(req.sloppak_out).expanduser()
        if not out.is_absolute():
            raise HTTPException(400, "sloppak_out must be an absolute path")

    with _JOBS_LOCK:
        running = [j for j in _JOBS.values() if j.state in ("starting", "running")]
        if len(running) >= MAX_RUNNING_JOBS:
            raise HTTPException(409, f"job {running[0].id} is still running "
                                     f"(one concurrent conversion job allowed)")
        job_id = uuid.uuid4().hex[:8]

    cmd = [str(PIPELINE_PY), str(orch), "--gp5-dir", str(gp5_dir)]
    if req.midi_src:
        cmd += ["--midi-src", str(Path(req.midi_src).expanduser())]
    if req.sloppak_out and not req.skip_sloppak:
        cmd += ["--sloppak-out", str(Path(req.sloppak_out).expanduser())]
    if req.skip_midi:
        cmd.append("--skip-midi")
    if req.skip_sloppak:
        cmd.append("--skip-sloppak")
    if req.overwrite_midi:
        cmd.append("--overwrite-midi")
    if req.force_sloppak:
        cmd.append("--force-sloppak")
    cmd += ["--jobs", str(req.jobs), "--sloppak-workers", str(req.sloppak_workers)]
    if req.midi_python:
        cmd += ["--midi-python", req.midi_python]
    if req.sloppak_python:
        cmd += ["--sloppak-python", req.sloppak_python]

    job = Job(job_id, req.model_dump(), cmd)
    try:
        job.proc = subprocess.Popen(
            cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, bufsize=1, start_new_session=True,
        )
    except OSError as e:
        raise HTTPException(500, f"failed to launch orchestrator: {e}")

    job.state = "running"
    with _JOBS_LOCK:
        _JOBS[job_id] = job
    threading.Thread(target=_reader_loop, args=(job,), daemon=True).start()
    return {"job_id": job_id, "cmd": cmd}


@router.get("/jobs")
def list_jobs():
    with _JOBS_LOCK:
        return [j.snapshot() for j in
                sorted(_JOBS.values(), key=lambda x: x.created, reverse=True)]


@router.get("/job/{job_id}")
def job_status(job_id: str, log_offset: int = 0):
    with _JOBS_LOCK:
        job = _JOBS.get(job_id)
    if not job:
        raise HTTPException(404, f"no such job: {job_id}")
    return job.snapshot(log_offset)


@router.post("/job/{job_id}/cancel")
def cancel_job(job_id: str):
    with _JOBS_LOCK:
        job = _JOBS.get(job_id)
    if not job:
        raise HTTPException(404, f"no such job: {job_id}")
    if job.state not in ("starting", "running"):
        return {"state": job.state}
    job.cancelled = True
    try:
        if job.proc and job.proc.poll() is None:
            # kill the whole process group (orchestrator + fluidsynth children)
            os.killpg(os.getpgid(job.proc.pid), signal.SIGTERM)
    except ProcessLookupError:
        pass
    return {"state": "cancelling"}


@router.get("/browse")
def browse(path: str | None = None):
    """Read-only directory listing for the folder-picker fallback (browser mode)."""
    home = Path.home()
    target = home if not path else Path(path).expanduser()
    try:
        target = target.resolve()
    except OSError:
        raise HTTPException(400, "invalid path")
    if not target.is_dir():
        raise HTTPException(404, "not a directory")
    dirs = []
    try:
        for entry in sorted(target.iterdir(), key=lambda p: p.name.lower()):
            if entry.is_dir() and not entry.name.startswith("."):
                try:
                    if os.access(entry, os.R_OK):
                        dirs.append({"name": entry.name,
                                     "path": str(entry),
                                     "midi_count": sum(
                                         1 for f in entry.iterdir()
                                         if f.suffix.lower() in (".mid", ".midi"))})
                except OSError:
                    continue
    except PermissionError:
        raise HTTPException(403, "permission denied")
    return {"path": str(target), "parent": str(target.parent) if str(target) != str(target.parent) else None,
            "dirs": dirs}
