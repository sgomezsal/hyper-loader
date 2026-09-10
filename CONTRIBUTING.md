# Contributing to hyper-loader

Thanks for considering a contribution. This is a small, focused tool, so the
bar for changes is "does it make the overlay more useful or more correct
without adding complexity that only one setup needs."

## Development setup

With Nix (recommended — pulls in the exact Rust toolchain and Wayland/xkb
headers used in CI):

```bash
nix develop
cargo build
```

Without Nix, install `pkg-config`, `wayland` and `libxkbcommon` development
headers via your distro's package manager, then:

```bash
cargo build
```

## Before opening a PR

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo build --release
```

These three are exactly what CI runs (see `.github/workflows/ci.yml`), so a
clean run locally means a clean run in CI.

There's no automated test suite — this is a GUI overlay driven by real
compositor state (surface configure events, shm buffers, a live desktop to
screenshot), which doesn't lend itself to unit tests. **Test manually** by
running the binary under a wlroots compositor and visually confirming your
change:

```bash
./target/release/hyper-loader --accent 7ed4c8 --gif examples/assets/work.gif
```

Let it run for a few seconds, check the effect you changed, then Ctrl-C it.
If your change touches the background-blur path, also test the full flow
from `examples/switch-specialisation.sh` (`grim` a screenshot to
`/tmp/hyper-loader-bg.png` before launching).

If your change touches per-output logic (`OutputSurface`, `new_output`,
`render_output`), test with more than one output. On Hyprland you can add a
throwaway virtual one without any extra hardware:

```bash
hyprctl output create headless
# ... test ...
hyprctl output remove HEADLESS-2   # or whatever name it got
```

Then `grim -o <name>` each output individually to check both actually got
an overlay.

## Scope

Good contributions:
- Bug fixes for rendering, color, or timing issues
- Support for another wlroots compositor quirk
- New decoration options that stay behind a flag (default behavior shouldn't
  change without discussion)
- Documentation and packaging (AUR, other distros)

Out of scope (open an issue to discuss first): new rendering backends,
config hot-reload, anything that turns this into a general-purpose
notification/widget system. The point of `hyper-loader` is staying a single
small binary you point a CLI flag (or a config file) at.

### Good first contributions

- **Per-output accent override.** Right now decorative `Settings` are
  shared across every output; a config table keyed by output name (e.g. an
  `[outputs."DP-4"]` section overriding `accent`) would let ultrawide/laptop
  combos look different per screen. Touches `resolve_settings` and
  `render_output` in `src/main.rs`.
- **AUR submission.** `packaging/PKGBUILD` is ready; it just needs a tagged
  release to point at and someone to push it to the AUR.
- **Other distro packaging** (Fedora COPR, Debian, openSUSE OBS, ...).

## Commit / PR style

- Keep commits focused; explain *why* in the message, not just *what*.
- Reference the behavior you tested manually in the PR description (there's
  no CI screenshot, so a one-line "tested with `--glitch` on Hyprland
  0.4x" is genuinely useful to reviewers).

## License

By contributing, you agree your changes are licensed under the project's
[MIT license](LICENSE).
