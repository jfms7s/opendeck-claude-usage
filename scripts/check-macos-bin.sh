#!/usr/bin/env bash
# Checks a macOS release binary: an Apple Silicon Mach-O, ad-hoc signed, and
# able to start. Run by ci.yml's `macos` job and release.yml's `build-macos`
# job; keep every macOS binary check here so the two can't drift.
# Usage: scripts/check-macos-bin.sh <path-to-binary>
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <path-to-binary>" >&2
  exit 2
fi
bin=$1

file "$bin"
# macOS's file says "executable arm64", Linux's "arm64 executable".
file -b "$bin" | grep -Eq '^Mach-O 64-bit (arm64 executable|executable arm64)'
# Apple Silicon kills unsigned binaries; the linker's ad-hoc
# signature must survive `strip = true`.
codesign --verify --verbose "$bin"
# Without OpenDeck's arguments it stops at the first missing flag,
# which proves it got as far as main().
out=$("$bin" 2>&1 || true)
echo "$out"
echo "$out" | grep -q "missing CLI flag: -port"
