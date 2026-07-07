#!/usr/bin/env bash
# Dev launcher for the slopsmith FastAPI server (+ pytest snippets).
# All paths are relative to this script, so it runs from any cwd.
set -euo pipefail

cd "$(dirname "$0")"

# Run all tests:        pytest
# Specific file:        pytest tests/test_song.py -v
# Pattern match:        pytest -k "round_trip" -v

PYTHONPATH="$PWD:$PWD/lib" .venv/bin/uvicorn server:app --host 0.0.0.0 --port 8001
