#!/usr/bin/env bash
# Dev launcher for the slopsmith FastAPI server (+ pytest snippets).
# All paths are relative to this script, so it runs from any cwd.
set -euo pipefail

cd "$(dirname "$0")"

# Debug mode: DEBUG=1 enables auto-reload + debug-level logging.
#   DEBUG=1 ./server.sh
# Run all tests:        pytest
# Specific file:        pytest tests/test_song.py -v
# Pattern match:        pytest -k "round_trip" -v

UVICORN_ARGS=(--host 0.0.0.0 --port 8001)
if [[ "${DEBUG:-0}" == "1" ]]; then
    UVICORN_ARGS+=(--reload --log-level debug)
fi

PYTHONPATH="$PWD:$PWD/lib" .venv/bin/uvicorn server:app "${UVICORN_ARGS[@]}"
