# rust4slopsmith
Rust 后端保持在 /Users/mac/codes/slopSmith/rust4slopsmith 独立存在，可单独 cargo run 运行（监听 8001，复用现有 CONFIG_DIR/DLC_DIR。

Rust backend for [Slopsmith](../slopsmith) — a drop-in replacement for the Python
`server.py` + `lib/` FastAPI backend. Same HTTP + WebSocket contract, same
on-disk layout (CONFIG_DIR, caches, DLC dir, `config.json`), same external
binary deps (ffmpeg, vgmstream-cli, RsCli).

## Status

Wave 1 (skeleton + config + SQLite metadata DB + settings/version/scan endpoints)
is implemented. Later waves port the library scan, binary format cores (PSARC,
SNG), the song XML parser, the WebSocket highway, retune, art serving, and the
Python sidecar proxy for plugins + Guitar Pro import. See
`../.claude/plans/calm-snuggling-haven.md`.

## Run

```bash
cargo run
# → listens on 0.0.0.0:8001
```

Env vars (all optional, mirror the Python backend):
- `DLC_DIR` — folder of `.psarc` / `.sloppak` songs (else `config.json` `dlc_dir`).
- `CONFIG_DIR` — writable config/cache root (default `~/.local/share/rocksmith-cdlc`).
- `APP_VERSION` — overrides the `VERSION` file for `GET /api/version`.
- `RSCLI_PATH` — path to the RsCli binary (SNG↔XML compile).
- `SLOPSMITH_SIDECAR` — command to launch the Python plugin/GP sidecar.

## Scope

Core server + the 8 library modules in its transitive closure
(`psarc`, `patcher`, `song`, `audio`, `tunings`, `sloppak`, `retune`,
`sng_vocals`). GP import, MIDI import, CDLC builder, sloppak conversion, and
Demucs stay in the Python sidecar (reached only via plugin routes).
