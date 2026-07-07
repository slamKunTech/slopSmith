#!/bin/bash

# Build the JUCE audio engine as a Node.js native addon
# Usage: ./scripts/build-audio.sh [debug|release]

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

BUILD_TYPE="${1:-Release}"

cd "$PROJECT_DIR"

# Sanitize the compiler environment on macOS. Homebrew's llvm on PATH
# exports CPLUS_INCLUDE_PATH/CXXFLAGS that prepend its libc++ headers ahead
# of the SDK's, clashing with Apple clang and producing
# "<cwchar> tried including <wchar.h> but didn't find libc++'s <wchar.h>"
# even when CC=/usr/bin/clang is set. These vars are irrelevant to the JUCE
# audio build (wasmedge/llvm paths belong to other projects), so strip them
# and prefer Apple clang unless the caller set CC/CXX explicitly.
if [ "$(uname -s)" = "Darwin" ]; then
    unset CPLUS_INCLUDE_PATH C_INCLUDE_PATH CXXFLAGS CFLAGS CPPFLAGS
    [ -z "${CC:-}" ] && export CC=/usr/bin/clang
    [ -z "${CXX:-}" ] && export CXX=/usr/bin/clang++
fi

# Switching build types requires a fresh configure — cmake-js reuses the
# cached CMAKE_BUILD_TYPE otherwise ("ninja: no work to do" leaves the prior
# .node in place and the -C CMAKE_BUILD_TYPE flag is silently ignored).
# If the cache's build type differs from the requested one, wipe build/.
if [ -f "build/CMakeCache.txt" ]; then
    CACHED_TYPE=$(grep -E "^CMAKE_BUILD_TYPE:STRING=" build/CMakeCache.txt \
        | cut -d= -f2 | tr -d ' ')
    if [ -n "$CACHED_TYPE" ] && [ "$CACHED_TYPE" != "$BUILD_TYPE" ]; then
        echo "Build type changed ($CACHED_TYPE -> $BUILD_TYPE); wiping build/ to reconfigure."
        rm -rf build
    fi
fi

# Ensure JUCE submodule is available
if [ ! -f "JUCE/CMakeLists.txt" ]; then
    echo "Initializing JUCE submodule..."
    git submodule update --init --recursive
fi

# Ensure node_modules exist (for node-addon-api headers)
if [ ! -d "node_modules" ]; then
    echo "Installing npm dependencies..."
    npm install
fi

# Detect architecture
ARCH=$(uname -m)
case "$ARCH" in
    x86_64)
        CMAKE_ARCH="x64"
        ;;
    aarch64|arm64)
        CMAKE_ARCH="arm64"
        ;;
    *)
        CMAKE_ARCH="$ARCH"
        ;;
esac

# Get the Electron version directly from the installed Electron package.
# Native addons MUST be built against the exact Electron ABI that ships
# with the app — guessing a fallback (the prior `|| echo 35.7.5`) can
# produce a .node that loads but crashes at runtime when the actual
# Electron version differs.
echo "Detecting Electron version..."
ELECTRON_PKG="node_modules/electron/package.json"
if [[ ! -f "$ELECTRON_PKG" ]]; then
    echo "Error: $ELECTRON_PKG not found. Run \`npm install\` before building native addons." >&2
    exit 1
fi
ELECTRON_VERSION=$(node -p "require('./$ELECTRON_PKG').version" 2>/dev/null | tr -d '\r\n')
if [[ -z "$ELECTRON_VERSION" ]]; then
    echo "Error: failed to read Electron version from $ELECTRON_PKG." >&2
    exit 1
fi
echo "  Electron version: $ELECTRON_VERSION"

# Set environment variables for cmake-js
# CROSS-PLATFORM NOTE: cmake-js looks for these CMAKE_JS_* variables internally
export CMAKE_JS_RUNTIME="electron"
export CMAKE_JS_RUNTIME_VERSION="$ELECTRON_VERSION"
export CMAKE_JS_ARCH="$CMAKE_ARCH"

# Also set npm_config variables for compatibility
# CROSS-PLATFORM NOTE: These are needed because cmake-js falls back to node-gyp
# which expects npm_config_* variables. Both sets are required for reliable
# cross-platform builds, especially on Windows where environment handling differs.
export npm_config_runtime="electron"
export npm_config_target="$ELECTRON_VERSION"
export npm_config_arch="$CMAKE_ARCH"
export npm_config_target_arch="$CMAKE_ARCH"

# Optional: clear cmake-js cache on Windows (where this matters most)
# CROSS-PLATFORM NOTE: Only clear cache in CI environments by default to avoid
# permission issues on local Windows machines and preserve incremental builds.
# On Windows, cmake-js downloads headers to a different location
# (C:\Users\...\.cmake-js) than on Unix systems.
# To force cache clearing locally, set CLEAN_CMAKE_JS=1
if [ "${CLEAN_CMAKE_JS:-}" = "1" ] || { [ -n "$CI" ] && [ -d "$HOME/.cmake-js" ]; }; then
  echo "Clearing cmake-js cache..."
  rm -rf "$HOME/.cmake-js"
fi

echo ""
echo "Building audio engine..."
echo "  Platform: $(uname -s)"
echo "  Arch: $CMAKE_ARCH"
echo "  Electron: $ELECTRON_VERSION"
echo "  Build type: $BUILD_TYPE"
echo ""

# Debug: show what cmake-js will see
echo "Environment for cmake-js:"
echo "  CMAKE_JS_RUNTIME=$CMAKE_JS_RUNTIME"
echo "  CMAKE_JS_RUNTIME_VERSION=$CMAKE_JS_RUNTIME_VERSION"
echo "  CMAKE_JS_ARCH=$CMAKE_JS_ARCH"
echo "  npm_config_runtime=$npm_config_runtime"
echo "  npm_config_target=$npm_config_target"
echo ""

npx cmake-js build \
    --runtime electron \
    --runtime-version "$ELECTRON_VERSION" \
    --arch "$CMAKE_ARCH" \
    --CDCMAKE_BUILD_TYPE="$BUILD_TYPE"

echo ""
echo "Build complete!"
if [ -f "build/Release/slopsmith_audio.node" ]; then
    echo "Output: build/Release/slopsmith_audio.node"
    ls -lh "build/Release/slopsmith_audio.node"
else
    echo "Warning: slopsmith_audio.node not found in expected location"
    find build -name "*.node" 2>/dev/null
fi
