# hyper-loader

A native Wayland overlay, built on `wlr-layer-shell`, meant to be shown while
a long-running command executes — the motivating use case is masking the
few seconds a `nixos-rebuild switch --specialisation <name>` takes, so
switching between NixOS specialisations feels instant instead of leaving you
staring at a frozen desktop.

It draws a fullscreen overlay layer (`Layer::Overlay`, no keyboard
interactivity, `exclusive_zone = -1`) with:

- a blurred screenshot of the desktop as a backdrop (fed in via a watched
  file, see below), fading in under the overlay decorations
- an animated grid, corner brackets and pulsing rings in a configurable
  accent color
- an optional looping GIF centered on screen
- an optional scrolling "digit rain" counter
- an optional scanline/glitch flicker effect

It works with any wlroots-based compositor that implements
`zwlr_layer_shell_v1` (Hyprland, Sway, river, …) — nothing here is
Hyprland-specific.

## Usage

```
hyper-loader [OPTIONS]

Options:
  --accent <HEX>          Primary accent color, e.g. "7ed4c8" (default: 7ed4c8)
  --accent2 <HEX>          Secondary accent for grid/corners/pulse (default: same as --accent)
  --gif <PATH>              GIF to loop in the center of the overlay
  --fade-ms <MS>            Fade-in duration for the decorations (default: 300)
  --no-counter               Disable the scrolling digit counter
  --glitch                    Enable scanline + glitch flicker over the background
  --overlay-alpha <0.0-1.0>  Darkness of the overlay drawn over the blurred screenshot (default: 0.55)
```

### Feeding it a background screenshot

`hyper-loader` polls for a file at `/tmp/hyper-loader-bg.png`. If present when
it starts, it loads, box-blurs, and fades it in as the backdrop, then deletes
the file. Typical usage is to `grim` a screenshot right before launching it:

```bash
grim /tmp/hyper-loader-bg.png
hyper-loader --accent 7ed4c8 &
LOADER_PID=$!
# ... wait for /tmp/hyper-loader-ready to appear, run your real command ...
kill "$LOADER_PID"
```

See [`examples/switch-specialisation.sh`](examples/switch-specialisation.sh)
for a complete example wiring this into `nixos-rebuild switch --specialisation`,
including per-specialisation accent colors.

### Readiness signal

Once the layer surface is configured (i.e. actually visible), `hyper-loader`
writes `/tmp/hyper-loader-ready`. Callers that spawn it in the background can
poll for that file to know it's safe to start the real work without a visible
flash of the plain desktop.

## Building

With Nix:

```bash
nix build
./result/bin/hyper-loader --help
```

Or with Cargo directly (needs `wayland` and `libxkbcommon` dev headers, plus
`pkg-config`):

```bash
cargo build --release
```

## Why not just a shell script + `swaylock`-style tool?

Because the point is a *non-blocking*, click-through overlay that composites
live over your desktop while a command runs in the background — not a lock
screen. The blur-then-fade-in of the actual desktop is what makes the
transition feel intentional rather than like a frozen screen.

## License

MIT — see [LICENSE](LICENSE).
