use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};

use clap::Parser;
use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder, RgbaImage};
use serde::Deserialize;
use std::fs::File;
use std::io::BufReader;
use tiny_skia::*;

const BG_WATCH_PATH: &str = "/tmp/hyper-loader-bg.png";
const READY_PATH: &str = "/tmp/hyper-loader-ready";
const DEFAULT_POOL_DIMENSIONS: (u32, u32) = (1920, 1080);

type Rgb = (f32, f32, f32);

fn parse_hex(s: &str) -> Rgb {
    let s = s.trim_start_matches('#');
    let bytes = u32::from_str_radix(s, 16).unwrap_or(0x7ed4c8);
    let r = ((bytes >> 16) & 0xFF) as f32 / 255.0;
    let g = ((bytes >> 8) & 0xFF) as f32 / 255.0;
    let b = (bytes & 0xFF) as f32 / 255.0;
    (r, g, b)
}

/// A native Wayland (wlr-layer-shell) fullscreen overlay meant to be shown
/// while a NixOS `nixos-rebuild switch` (or any long-running command) runs,
/// e.g. when switching between specialisations.
///
/// Every option below can also be set in a config file (CLI flags win when
/// both are given) — see `--config` and the "config file" section of the
/// README.
#[derive(Parser)]
#[command(name = "hyper-loader")]
struct Args {
    /// Primary accent color (hex, e.g. "7ed4c8") used for the grid, corners,
    /// pulse rings and the digit counter. [config default: 7ed4c8]
    #[arg(long)]
    accent: Option<String>,

    /// Secondary accent color for grid/corner/pulse decorations. Defaults to
    /// --accent; set it to something else for a contrasting look.
    #[arg(long)]
    accent2: Option<String>,

    /// Fade-in duration in milliseconds for the overlay decorations. [config default: 300]
    #[arg(long)]
    fade_ms: Option<u64>,

    /// Optional GIF to loop in the center of the overlay.
    #[arg(long)]
    gif: Option<PathBuf>,

    /// Disable the scrolling digit-counter overlay.
    #[arg(long)]
    no_counter: bool,

    /// Enable a scanline + glitch flicker effect over the background.
    #[arg(long)]
    glitch: bool,

    /// Alpha (0.0-1.0) of the dark overlay drawn over the blurred screenshot
    /// background. Out-of-range values are clamped. [config default: 0.55]
    #[arg(long)]
    overlay_alpha: Option<f32>,

    /// Path to a TOML config file. Defaults to
    /// $XDG_CONFIG_HOME/hyper-loader/config.toml (or
    /// ~/.config/hyper-loader/config.toml). Missing default config is not
    /// an error; a missing file passed explicitly here is.
    #[arg(long)]
    config: Option<PathBuf>,
}

/// Mirrors `Args`, minus `--config` itself. Every field is optional so a
/// config file only needs to set what it wants to override.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    accent: Option<String>,
    accent2: Option<String>,
    fade_ms: Option<u64>,
    gif: Option<PathBuf>,
    no_counter: Option<bool>,
    glitch: Option<bool>,
    overlay_alpha: Option<f32>,
}

fn default_config_path() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("hyper-loader/config.toml"));
        }
    }
    std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".config/hyper-loader/config.toml"))
}

fn load_config(explicit_path: Option<&Path>) -> FileConfig {
    let (path, is_explicit) = match explicit_path {
        Some(p) => (p.to_path_buf(), true),
        None => match default_config_path() {
            Some(p) => (p, false),
            None => return FileConfig::default(),
        },
    };

    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
            eprintln!(
                "hyper-loader: failed to parse config {}: {e}",
                path.display()
            );
            FileConfig::default()
        }),
        Err(_) if is_explicit => {
            eprintln!("hyper-loader: config file {} not found", path.display());
            FileConfig::default()
        }
        Err(_) => FileConfig::default(),
    }
}

/// Resolved, render-ready settings: CLI flags override the config file,
/// which overrides these hardcoded defaults.
#[derive(Clone, Copy)]
struct Settings {
    accent: Rgb,
    accent2: Rgb,
    glitch: bool,
    overlay_alpha: f32,
    fade_ms: f32,
    no_counter: bool,
}

fn resolve_settings(args: &Args, config: &FileConfig) -> (Settings, Option<PathBuf>) {
    let accent_hex = args
        .accent
        .clone()
        .or_else(|| config.accent.clone())
        .unwrap_or_else(|| "7ed4c8".to_string());
    let accent = parse_hex(&accent_hex);

    let accent2 = args
        .accent2
        .clone()
        .or_else(|| config.accent2.clone())
        .as_deref()
        .map(parse_hex)
        .unwrap_or(accent);

    let fade_ms = args.fade_ms.or(config.fade_ms).unwrap_or(300) as f32;
    let gif = args.gif.clone().or_else(|| config.gif.clone());
    let no_counter = args.no_counter || config.no_counter.unwrap_or(false);
    let glitch = args.glitch || config.glitch.unwrap_or(false);
    let overlay_alpha = args
        .overlay_alpha
        .or(config.overlay_alpha)
        .unwrap_or(0.55)
        .clamp(0.0, 1.0);

    (
        Settings {
            accent,
            accent2,
            glitch,
            overlay_alpha,
            fade_ms,
            no_counter,
        },
        gif,
    )
}

type SharedBg = Arc<Mutex<Option<RgbaImage>>>;

/// Per-output layer surface state. `hyper-loader` creates one of these for
/// every `wl_output` it sees, so the overlay covers every connected
/// monitor rather than whichever one the compositor happens to pick.
struct OutputSurface {
    output: wl_output::WlOutput,
    layer_surface: LayerSurface,
    pool: SlotPool,
    width: u32,
    height: u32,
    configured: bool,
}

struct AppState {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor_state: CompositorState,
    shm: Shm,
    layer_shell: LayerShell,
    outputs: Vec<OutputSurface>,
    settings: Settings,
    background: SharedBg,
    bg_fade_start: Option<Instant>,
    gif_frames: Vec<(RgbaImage, u32)>,
    start_time: Instant,
    counter_seed: u64,
    exit: bool,
}

impl CompositorHandler for AppState {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        self.counter_seed = self
            .counter_seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);

        if self.bg_fade_start.is_none() {
            let has_bg = self.background.lock().unwrap().is_some();
            if has_bg {
                self.bg_fade_start = Some(Instant::now());
            }
        }

        if let Some(idx) = self
            .outputs
            .iter()
            .position(|o| o.layer_surface.wl_surface() == surface)
        {
            self.render_output(qh, idx);
        }
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for AppState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    /// Fired once per output — both for the outputs already present at
    /// startup (after the initial roundtrip in `main`) and for anything
    /// hotplugged later. Each gets its own layer-shell surface pinned to
    /// that specific output via `create_layer_surface`'s output argument.
    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        let surface = self.compositor_state.create_surface(qh);
        let layer_surface = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Overlay,
            Some("hyper-loader"),
            Some(&output),
        );
        layer_surface.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer_surface.set_size(0, 0);
        layer_surface.commit();

        let (pw, ph) = self
            .output_state
            .info(&output)
            .and_then(|info| info.modes.iter().find(|m| m.current).cloned())
            .map(|m| (m.dimensions.0.max(1) as u32, m.dimensions.1.max(1) as u32))
            .unwrap_or(DEFAULT_POOL_DIMENSIONS);

        // Oversize a bit so a `configure` slightly larger than the reported
        // mode (fractional scaling, compositor quirks) doesn't fail to
        // allocate a buffer.
        let pool_size = (pw as usize) * (ph as usize) * 4 * 2;
        let pool = SlotPool::new(pool_size, &self.shm).expect("failed to create shm pool");

        self.outputs.push(OutputSurface {
            output,
            layer_surface,
            pool,
            width: pw,
            height: ph,
            configured: false,
        });
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.outputs.retain(|o| o.output != output);
        if self.outputs.is_empty() {
            self.exit = true;
        }
    }
}

impl LayerShellHandler for AppState {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        self.outputs
            .retain(|o| o.layer_surface.wl_surface() != layer.wl_surface());
        if self.outputs.is_empty() {
            self.exit = true;
        }
    }
    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let Some(idx) = self
            .outputs
            .iter()
            .position(|o| o.layer_surface.wl_surface() == layer.wl_surface())
        else {
            return;
        };

        let out = &mut self.outputs[idx];
        if configure.new_size.0 > 0 {
            out.width = configure.new_size.0;
        }
        if configure.new_size.1 > 0 {
            out.height = configure.new_size.1;
        }
        out.configured = true;
        let _ = std::fs::write(READY_PATH, "1");
        self.render_output(qh, idx);
    }
}

impl ShmHandler for AppState {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl AppState {
    fn render_output(&mut self, qh: &QueueHandle<Self>, idx: usize) {
        let t = self.start_time.elapsed().as_secs_f32();
        let fade_alpha = ((t * 1000.0) / self.settings.fade_ms).min(1.0);
        let bg_fade = self
            .bg_fade_start
            .map(|s| (s.elapsed().as_secs_f32() / 0.6).min(1.0))
            .unwrap_or(0.0);
        let seed = self.counter_seed;
        let settings = self.settings;

        let bg_guard = self.background.lock().unwrap();
        let bg = bg_guard.as_ref();

        let gif_frame = if !self.gif_frames.is_empty() {
            let total_ms: u32 = self.gif_frames.iter().map(|(_, d)| d).sum();
            let elapsed_ms = (t * 1000.0) as u32 % total_ms.max(1);
            let mut acc = 0u32;
            let mut idx = 0;
            for (i, (_, delay)) in self.gif_frames.iter().enumerate() {
                acc += delay;
                if elapsed_ms < acc {
                    idx = i;
                    break;
                }
            }
            Some(&self.gif_frames[idx].0)
        } else {
            None
        };

        let Some(out) = self.outputs.get_mut(idx) else {
            return;
        };
        if !out.configured {
            return;
        }

        let w = out.width;
        let h = out.height;

        let (buffer, canvas) = match out.pool.create_buffer(
            w as i32,
            h as i32,
            (w * 4) as i32,
            wl_shm::Format::Argb8888,
        ) {
            Ok(b) => b,
            Err(_) => return,
        };

        let mut pixmap = PixmapMut::from_bytes(canvas, w, h).unwrap();
        draw_frame(
            &mut pixmap,
            t,
            fade_alpha,
            bg_fade,
            settings.accent,
            settings.accent2,
            settings.glitch,
            settings.overlay_alpha,
            h,
            bg,
            gif_frame,
            seed,
            settings.no_counter,
        );

        drop(bg_guard);

        let surface = out.layer_surface.wl_surface().clone();
        surface.attach(Some(buffer.wl_buffer()), 0, 0);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        surface.frame(qh, surface.clone());
        surface.commit();
    }
}

fn box_blur(img: &RgbaImage, radius: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let mut out = img.clone();
    let r = radius as i32;
    let area = ((2 * r + 1) * (2 * r + 1)) as u64;
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let (mut rs, mut gs, mut bs) = (0u64, 0u64, 0u64);
            for dy in -r..=r {
                for dx in -r..=r {
                    let px = (x + dx).clamp(0, w as i32 - 1) as u32;
                    let py = (y + dy).clamp(0, h as i32 - 1) as u32;
                    let p = img.get_pixel(px, py);
                    rs += p[0] as u64;
                    gs += p[1] as u64;
                    bs += p[2] as u64;
                }
            }
            let p = out.get_pixel_mut(x as u32, y as u32);
            p[0] = (rs / area) as u8;
            p[1] = (gs / area) as u8;
            p[2] = (bs / area) as u8;
            p[3] = 255;
        }
    }
    out
}

fn lcg_f32(seed: u64, offset: u64) -> f32 {
    let s = seed.wrapping_add(offset.wrapping_mul(2891336453));
    ((s >> 33) as f32) / (u32::MAX as f32)
}

#[allow(clippy::too_many_arguments)]
fn draw_frame(
    pixmap: &mut PixmapMut,
    t: f32,
    fade_alpha: f32,
    bg_fade: f32,
    accent: Rgb,
    accent2: Rgb,
    glitch: bool,
    overlay_alpha: f32,
    height: u32,
    bg: Option<&RgbaImage>,
    gif_frame: Option<&RgbaImage>,
    seed: u64,
    no_counter: bool,
) {
    let w = pixmap.width() as f32;
    let h = pixmap.height() as f32;
    let pw = pixmap.width();
    let ph = pixmap.height();

    // The wl_shm Argb8888 buffer is read back by tiny-skia in B,G,R,A order,
    // so red and blue are swapped once here rather than at every call site
    // below (the background/GIF blits already do their own per-pixel swap).
    let (b, g, r) = accent;
    let (cb, cg, cr) = accent2;

    // --- Background phase 1: tinted dark base, derived from the accent color ---
    let base = Color::from_rgba(
        (r * 0.15 + 0.02).min(1.0),
        (g * 0.15 + 0.02).min(1.0),
        (b * 0.15 + 0.02).min(1.0),
        1.0,
    )
    .unwrap();
    pixmap.fill(base);

    // --- Background phase 2: blurred screenshot fading in ---
    if let Some(img) = bg {
        if bg_fade > 0.0 {
            let img_w = img.width();
            let img_h = img.height();
            let pixels = pixmap.pixels_mut();
            for py in 0..ph {
                for px in 0..pw {
                    let sx = (px as f32 * img_w as f32 / pw as f32) as u32;
                    let sy = (py as f32 * img_h as f32 / ph as f32) as u32;
                    let p = img.get_pixel(sx.min(img_w - 1), sy.min(img_h - 1));
                    let idx = (py * pw + px) as usize;
                    let existing = pixels[idx].demultiply();
                    let blend = bg_fade;
                    let nr =
                        p[2] as f32 / 255.0 * blend + existing.red() as f32 / 255.0 * (1.0 - blend);
                    let ng = p[1] as f32 / 255.0 * blend
                        + existing.green() as f32 / 255.0 * (1.0 - blend);
                    let nb = p[0] as f32 / 255.0 * blend
                        + existing.blue() as f32 / 255.0 * (1.0 - blend);
                    pixels[idx] = ColorU8::from_rgba(
                        (nb * 255.0) as u8,
                        (ng * 255.0) as u8,
                        (nr * 255.0) as u8,
                        255,
                    )
                    .premultiply();
                }
            }

            let mut overlay = Paint::default();
            overlay.set_color(Color::from_rgba(0.0, 0.0, 0.0, overlay_alpha * bg_fade).unwrap());
            overlay.blend_mode = BlendMode::SourceOver;
            pixmap.fill_rect(
                Rect::from_xywh(0.0, 0.0, w, h).unwrap(),
                &overlay,
                Transform::identity(),
                None,
            );

            if glitch {
                let mut tint = Paint::default();
                tint.set_color(Color::from_rgba(cr, cg, cb, 0.15 * bg_fade).unwrap());
                tint.blend_mode = BlendMode::SourceOver;
                pixmap.fill_rect(
                    Rect::from_xywh(0.0, 0.0, w, h).unwrap(),
                    &tint,
                    Transform::identity(),
                    None,
                );
            }
        }
    }

    let a = |base: f32| (base * fade_alpha).clamp(0.0, 1.0);

    // --- Grid ---
    let grid_size = 60.0f32;
    let offset = (t * 20.0) % grid_size;
    let mut grid_paint = Paint::default();
    grid_paint.set_color(Color::from_rgba(cr, cg, cb, a(0.06)).unwrap());
    let stroke = Stroke {
        width: 1.0,
        ..Default::default()
    };
    let mut x = -offset;
    while x < w {
        let mut pb = PathBuilder::new();
        pb.move_to(x, 0.0);
        pb.line_to(x, h);
        if let Some(path) = pb.finish() {
            pixmap.stroke_path(&path, &grid_paint, &stroke, Transform::identity(), None);
        }
        x += grid_size;
    }
    let mut y = -offset;
    while y < h {
        let mut pb = PathBuilder::new();
        pb.move_to(0.0, y);
        pb.line_to(w, y);
        if let Some(path) = pb.finish() {
            pixmap.stroke_path(&path, &grid_paint, &stroke, Transform::identity(), None);
        }
        y += grid_size;
    }

    // --- Corners ---
    let corner_alpha = a((0.6 + 0.2 * (t * 2.0).sin()).clamp(0.0, 1.0));
    let mut cp = Paint::default();
    cp.set_color(Color::from_rgba(cr, cg, cb, corner_alpha).unwrap());
    let cs = 60.0f32;
    let m = 30.0f32;
    let cw = 2.0f32;
    pixmap.fill_rect(
        Rect::from_xywh(m, m, cs, cw).unwrap(),
        &cp,
        Transform::identity(),
        None,
    );
    pixmap.fill_rect(
        Rect::from_xywh(m, m, cw, cs).unwrap(),
        &cp,
        Transform::identity(),
        None,
    );
    pixmap.fill_rect(
        Rect::from_xywh(w - m - cs, m, cs, cw).unwrap(),
        &cp,
        Transform::identity(),
        None,
    );
    pixmap.fill_rect(
        Rect::from_xywh(w - m - cw, m, cw, cs).unwrap(),
        &cp,
        Transform::identity(),
        None,
    );
    pixmap.fill_rect(
        Rect::from_xywh(m, h - m - cw, cs, cw).unwrap(),
        &cp,
        Transform::identity(),
        None,
    );
    pixmap.fill_rect(
        Rect::from_xywh(m, h - m - cs, cw, cs).unwrap(),
        &cp,
        Transform::identity(),
        None,
    );
    pixmap.fill_rect(
        Rect::from_xywh(w - m - cs, h - m - cw, cs, cw).unwrap(),
        &cp,
        Transform::identity(),
        None,
    );
    pixmap.fill_rect(
        Rect::from_xywh(w - m - cw, h - m - cs, cw, cs).unwrap(),
        &cp,
        Transform::identity(),
        None,
    );

    // --- Pulse rings ---
    for i in 0..3 {
        let phase = (t * 0.4 + i as f32 * 0.33) % 1.0;
        let radius = phase * (w.min(h) * 0.45);
        let alpha = a(((1.0 - phase) * 0.45).clamp(0.0, 1.0));
        let mut rp = Paint::default();
        rp.set_color(Color::from_rgba(cr, cg, cb, alpha).unwrap());
        rp.anti_alias = true;
        let rs = Stroke {
            width: 1.5,
            ..Default::default()
        };
        let mut pb = PathBuilder::new();
        pb.push_oval(
            Rect::from_xywh(
                w / 2.0 - radius,
                h / 2.0 - radius,
                radius * 2.0,
                radius * 2.0,
            )
            .unwrap(),
        );
        if let Some(path) = pb.finish() {
            pixmap.stroke_path(&path, &rp, &rs, Transform::identity(), None);
        }
    }

    // --- Centered GIF frame ---
    if let Some(gif) = gif_frame {
        let gw = gif.width() as f32;
        let gh = gif.height() as f32;
        let scale = 1.5f32;
        let dw = gw * scale;
        let dh = gh * scale;
        let ox = (w / 2.0 - dw / 2.0) as i32;
        let oy = (h / 2.0 - dh / 2.0 - 30.0) as i32;

        let pixels = pixmap.pixels_mut();
        for dy in 0..dh as i32 {
            for dx in 0..dw as i32 {
                let px = ox + dx;
                let py = oy + dy;
                if px < 0 || py < 0 || px >= pw as i32 || py >= ph as i32 {
                    continue;
                }
                let sx = (dx as f32 * gw / dw) as u32;
                let sy = (dy as f32 * gh / dh) as u32;
                let p = gif.get_pixel(sx.min(gif.width() - 1), sy.min(gif.height() - 1));
                let ga = p[3] as f32 / 255.0 * fade_alpha;
                if ga < 0.01 {
                    continue;
                }
                let idx = (py as u32 * pw + px as u32) as usize;
                let existing = pixels[idx].demultiply();
                let nr = (p[0] as f32 / 255.0 * ga + existing.red() as f32 / 255.0 * (1.0 - ga))
                    .clamp(0.0, 1.0);
                let ng = (p[1] as f32 / 255.0 * ga + existing.green() as f32 / 255.0 * (1.0 - ga))
                    .clamp(0.0, 1.0);
                let nb = (p[2] as f32 / 255.0 * ga + existing.blue() as f32 / 255.0 * (1.0 - ga))
                    .clamp(0.0, 1.0);
                pixels[idx] = ColorU8::from_rgba(
                    (nb * 255.0) as u8,
                    (ng * 255.0) as u8,
                    (nr * 255.0) as u8,
                    255,
                )
                .premultiply();
            }
        }
    }

    // --- Scrolling digit counter ---
    if !no_counter {
        let counter_y = h / 2.0 + 155.0;
        let num_cols = 8i32;
        let col_w = 40.0f32;
        let start_x = w / 2.0 - (num_cols as f32 * col_w) / 2.0;
        for col in 0..num_cols {
            let speed = 3.0 + lcg_f32(seed, col as u64 * 7) * 8.0;
            let digit_count = 6i32;
            let digit_h = 18.0f32;
            for row in 0..digit_count {
                let n = ((t * speed * 10.0) as u64 + col as u64 * 31 + row as u64 * 17 + seed) % 10;
                let alpha_row = if row == digit_count - 1 {
                    a(0.9)
                } else if row == digit_count - 2 {
                    a(0.5)
                } else {
                    a(0.12)
                };
                let digit_x = start_x + col as f32 * col_w;
                let digit_y = counter_y - row as f32 * digit_h;
                draw_pixel_digit(pixmap, n as u8, digit_x, digit_y, r, g, b, alpha_row);
            }
        }
    }

    // --- Optional glitch: scanlines + flicker ---
    if glitch {
        let scan_y = ((t * 180.0) as u32 % height.max(1)) as f32;
        let mut sp = Paint::default();
        sp.set_color(Color::from_rgba(cr, cg, cb, a(0.15)).unwrap());
        pixmap.fill_rect(
            Rect::from_xywh(0.0, scan_y, w, 2.0).unwrap(),
            &sp,
            Transform::identity(),
            None,
        );
        for i in 0..3u32 {
            let gy = ((t * 300.0 + i as f32 * 137.0) as u32 % height.max(1)) as f32;
            let gl = 50.0 + lcg_f32(seed, i as u64) * 200.0;
            let gx = lcg_f32(seed, i as u64 + 100) * (w - gl);
            let mut gp = Paint::default();
            gp.set_color(Color::from_rgba(cr, cg, cb, a(0.2)).unwrap());
            pixmap.fill_rect(
                Rect::from_xywh(gx, gy, gl, 1.0).unwrap(),
                &gp,
                Transform::identity(),
                None,
            );
        }
    }
}

const DIGITS: [[u8; 7]; 10] = [
    [
        0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
    ],
    [
        0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
    ],
    [
        0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111,
    ],
    [
        0b11111, 0b00010, 0b00100, 0b00110, 0b00001, 0b10001, 0b01110,
    ],
    [
        0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
    ],
    [
        0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
    ],
    [
        0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
    ],
    [
        0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
    ],
    [
        0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
    ],
    [
        0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
    ],
];

#[allow(clippy::too_many_arguments)]
fn draw_pixel_digit(
    pixmap: &mut PixmapMut,
    digit: u8,
    x: f32,
    y: f32,
    r: f32,
    g: f32,
    b: f32,
    alpha: f32,
) {
    if alpha < 0.01 {
        return;
    }
    let pw = pixmap.width() as f32;
    let ph = pixmap.height() as f32;
    let ps = 2.0f32;
    let d = &DIGITS[digit.min(9) as usize];
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba(r, g, b, alpha).unwrap());
    for (row, &bits) in d.iter().enumerate() {
        for col in 0..5u8 {
            if bits & (1 << (4 - col)) != 0 {
                let px = x + col as f32 * ps;
                let py = y + row as f32 * ps;
                if px >= 0.0 && py >= 0.0 && px < pw && py < ph {
                    if let Some(rect) = Rect::from_xywh(px, py, ps, ps) {
                        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
                    }
                }
            }
        }
    }
}

delegate_compositor!(AppState);
delegate_output!(AppState);
delegate_shm!(AppState);
delegate_layer!(AppState);
delegate_registry!(AppState);

impl ProvidesRegistryState for AppState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

fn main() {
    let args = Args::parse();
    let config = load_config(args.config.as_deref());
    let (settings, gif_path) = resolve_settings(&args, &config);

    let gif_frames: Vec<(RgbaImage, u32)> = gif_path
        .as_ref()
        .and_then(|p| {
            File::open(p).ok().map(|f| {
                let reader = BufReader::new(f);
                GifDecoder::new(reader)
                    .ok()
                    .and_then(|dec| dec.into_frames().collect_frames().ok())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|frame| {
                        let delay = frame.delay().numer_denom_ms().0.max(50);
                        (frame.into_buffer(), delay)
                    })
                    .collect()
            })
        })
        .unwrap_or_default();

    let background: SharedBg = Arc::new(Mutex::new(None));

    {
        let bg_shared = Arc::clone(&background);
        thread::spawn(move || {
            let _ = std::fs::remove_file(BG_WATCH_PATH);
            loop {
                thread::sleep(Duration::from_millis(50));
                if std::path::Path::new(BG_WATCH_PATH).exists() {
                    if let Ok(img) = image::open(BG_WATCH_PATH) {
                        let rgba = img.to_rgba8();
                        let b1 = box_blur(&rgba, 14);
                        let b2 = box_blur(&b1, 12);
                        let blurred = box_blur(&b2, 10);
                        *bg_shared.lock().unwrap() = Some(blurred);
                        let _ = std::fs::remove_file(BG_WATCH_PATH);
                    }
                    break;
                }
            }
        });
    }

    let conn = Connection::connect_to_env().expect("Failed to connect to Wayland");
    let (globals, mut event_queue) = registry_queue_init(&conn).unwrap();
    let qh = event_queue.handle();

    let compositor_state = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm");
    let output_state = OutputState::new(&globals, &qh);
    let layer_shell = LayerShell::bind(&globals, &qh).expect("zwlr_layer_shell_v1");

    let mut app = AppState {
        registry_state: RegistryState::new(&globals),
        output_state,
        compositor_state,
        shm,
        layer_shell,
        outputs: Vec::new(),
        settings,
        background,
        bg_fade_start: None,
        gif_frames,
        start_time: Instant::now(),
        counter_seed: 12345678901234567,
        exit: false,
    };

    // Outputs already connected at startup are announced as globals during
    // `registry_queue_init`, but `OutputState` only fires `new_output` once
    // each output's info (geometry/mode/done) has actually been dispatched.
    // Round-tripping here guarantees every currently-connected monitor has
    // a layer surface before the first render, instead of only whichever
    // output happens to get picked implicitly.
    event_queue.roundtrip(&mut app).unwrap();

    let mut event_loop: EventLoop<AppState> = EventLoop::try_new().unwrap();
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .unwrap();

    loop {
        event_loop
            .dispatch(Some(Duration::from_millis(16)), &mut app)
            .unwrap();
        if app.exit {
            break;
        }
    }

    let _ = std::fs::remove_file(READY_PATH);
}
