#!/bin/bash
# Local macOS release build. Uses scripts/build-macos.sh — NOT
# `npm run dist:mac`, whose `bundle.sh` is Linux-only (skips python/bin
# bundling on mac) and whose `build:native` re-run is destructive on mac
# (see build-common.sh package_application comment).
#
# Env baked in for this machine:
#   • stable .NET 10 SDK at ~/.dotnet10-stable (10.0.301) — the system
#     `dotnet` is a preview SDK that corrupts RsCli SNG output
#     (byte 0 != 0x4a). See memory rscli-build-stable-sdk.
#   • gh-proxy mirror for the rs2014net clone (build:rscli) — direct
#     GitHub times out / HTTP2-framing-errors here.
#   • sibling slopsmith checkout (../slopsmith) instead of a fresh clone.
#
# --no-notarize (or NO_NOTARIZE=1): skip macOS notarization for local
# builds without Apple Developer credentials. Produces an unsigned,
# unnotarized .dmg that runs on this machine (right-click → Open the
# first time) but is blocked by Gatekeeper on other Macs.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$SCRIPT_DIR"

# Stable .NET 10 SDK (not the system preview).
export DOTNET_ROOT="${HOME}/.dotnet10-stable"
export DOTNET_MULTILEVEL_LOOKUP=0
export PATH="${DOTNET_ROOT}:${PATH}"

# GitHub mirror for rs2014net (build:rscli clone).
export RS2014_GIT_URL="https://gh-proxy.com/https://github.com/iminashi/Rocksmith2014.NET.git"

# Reuse the sibling slopsmith checkout (skip clone_slopsmith's network clone).
export SLOPSMITH_DIR="${PROJECT_DIR}/../slopsmith"

# --no-notarize flag → NO_NOTARIZE env (honored by build-macos.sh).
ARGS=()
for arg in "$@"; do
    case "$arg" in
        --no-notarize) export NO_NOTARIZE=1 ;;
        *) ARGS+=("$arg") ;;
    esac
done

bash "${PROJECT_DIR}/scripts/build-macos.sh" "${ARGS[@]}"
