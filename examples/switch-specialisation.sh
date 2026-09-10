#!/usr/bin/env bash
# Example: show a hyper-loader overlay while nixos-rebuild switches
# to a different NixOS specialisation.
#
# Usage: switch-specialisation.sh <specialisation-name> [flake-path]
#
# Customize ACCENT/GLITCH/GIF per specialisation to taste — this is just
# a starting point, not a generic tool. The GIFs referenced below live in
# examples/assets/ in this repo and are only here so you have something to
# look at on the first run; swap in whatever suits your setup (or drop
# --gif entirely).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SPEC="${1:?usage: switch-specialisation.sh <specialisation-name> [flake-path]}"
FLAKE="${2:-$PWD}"

case "$SPEC" in
  work)  ACCENT="ffffff"; GLITCH="";        GIF="$SCRIPT_DIR/assets/work.gif" ;;
  relax) ACCENT="b072d1"; GLITCH="";        GIF="$SCRIPT_DIR/assets/relax.gif" ;;
  anon)  ACCENT="ff2222"; GLITCH="--glitch"; GIF="$SCRIPT_DIR/assets/anon.gif" ;;
  *)     ACCENT="7ed4c8"; GLITCH="";        GIF="" ;;
esac

rm -f /tmp/hyper-loader-ready
grim /tmp/hyper-loader-bg.png

hyper-loader --accent "$ACCENT" $GLITCH ${GIF:+--gif "$GIF"} &
LOADER_PID=$!

timeout 2 bash -c 'until [ -f /tmp/hyper-loader-ready ]; do sleep 0.02; done'

if [ "$SPEC" = "default" ]; then
  sudo nixos-rebuild switch --flake "$FLAKE"
else
  sudo nixos-rebuild switch --flake "$FLAKE" --specialisation "$SPEC"
fi

sleep 1.5
kill "$LOADER_PID" 2>/dev/null || true
wait "$LOADER_PID" 2>/dev/null || true

notify-send "hyper-loader" "$SPEC ready" || true
rm -f /tmp/hyper-loader-bg.png /tmp/hyper-loader-ready
