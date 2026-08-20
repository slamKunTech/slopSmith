#!/bin/bash
set -e

# Navigate to slopsmith backend directory
cd /Users/mac/codes/slopSmith/slopsmith

# Kill any existing servers
echo "Cleaning up existing servers..."
lsof -ti:8000 | xargs kill -9 2>/dev/null || true
lsof -ti:8081 | xargs kill -9 2>/dev/null || true

# Start backend API/WebSocket server on port 8000 (single process —
# uvicorn also serves the UI at / and /static, Free Play included)
echo "Starting backend server on port 8000..."
./.venv/bin/python -m uvicorn server:app --host 0.0.0.0 --port 8000 --reload &
BACKEND_PID=$!

# Wait for server to be ready
echo "Waiting for server to start..."
sleep 3

# Check if server is running
if ! lsof -ti:8000 > /dev/null; then
    echo "ERROR: Backend server failed to start on port 8000"
    exit 1
fi

echo "✓ Backend server running on http://localhost:8000 (PID: $BACKEND_PID)"

# Open Chrome with developer tools
echo "Opening Chrome with FreePlay page..."
open -a "Google Chrome" "http://localhost:8000/"

echo ""
echo "=== Debug Mode Active ==="
echo "Backend: http://localhost:8000  (Free Play is the top-bar link)"
echo "Press Ctrl+C to stop the server"
echo ""

# Wait for Ctrl+C
trap "echo 'Stopping server...'; kill $BACKEND_PID 2>/dev/null; exit 0" INT
wait
