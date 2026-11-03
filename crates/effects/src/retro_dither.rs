//! Retro Dither: an original EffectCraft effect (Stylize), not an After Effects one. It reduces a
//! picture to a small palette the way old computers and consoles had to, with ordered or
//! error-diffusion dithering, optionally on big chunky pixels.
//!
//! Written from the published feature lists of retro-dithering plug-ins (ordered Bayer and
//! error-diffusion methods, pixel downscaling, a threshold, serpentine scanning, grayscale
//! conversion, hardware palettes and palettes taken from a layer) and from the textbook
//! algorithms themselves; the code, parameters and maths are our own.
//!
//! * **Pixel Size** averages the picture into cells (aligned to the layer's origin), dithers the
//!   cells and draws each one as a solid block.
//! * **Ordered** methods add a threshold map (Bayer 2×2 / 4×4 / 8×8, a 4×4 clustered dot,
//!   interleaved gradient noise or white noise) scaled to the palette's spacing, then take the
//!   nearest palette colour. **Error diffusion** methods (Floyd–Steinberg, Jarvis–Judice–Ninke,
//!   Stucki, Atkinson, Burkes, Sierra, Two-Row Sierra, Sierra Lite, Stevenson–Arce) push each
//!   cell's quantisation error onto the cells not yet visited, scanning left to right or
//!   serpentine.
//! * **Palettes** are lists of colours (the hardware palettes are published facts about that
//!   hardware's output, approximated as sRGB) or per-channel levels (RGB 3-3-2, web safe, Levels),
//!   or come from a layer (median cut) or the eight Custom colours.
//! * Matching happens on display-encoded (sRGB) values, so a linear working space dithers like a
//!   display-referred one; alpha is kept.

use effectcraft_color::{linear_to_srgb, luminance, srgb_to_linear};
use effectcraft_keyframe::Value;
use effectcraft_project::ParamUi;
use effectcraft_raster::Px;

use crate::util::{premul, unpremul};
use crate::{Buf, EffectCtx, EffectSpec, col, num, p, popup, slider};

pub const ID: &str = "ec.stylize.retrodither";

/// Twirl-down groups (id, display name).
pub const GROUPS: &[(&str, &str)] = &[("rdCustom", "Custom Colors"), ("rdLayer", "Palette Layer")];

const METHODS: &[&str] = &[
    "None",
    "Bayer 2x2",
    "Bayer 4x4",
    "Bayer 8x8",
    "Clustered Dot 4x4",
    "Interleaved Gradient Noise",
    "White Noise",
    "Floyd-Steinberg",
    "Jarvis, Judice & Ninke",
    "Stucki",
    "Atkinson",
    "Burkes",
    "Sierra",
    "Two-Row Sierra",
    "Sierra Lite",
    "Stevenson-Arce",
];
/// First error-diffusion entry of [`METHODS`].
const DIFFUSION: u32 = 7;

const PALETTES: &[&str] = &[
    "Black & White",
    "Grayscale 4",
    "Grayscale 16",
    "Game Boy Greens",
    "CGA Cyan / Magenta",
    "CGA / EGA 16",
    "ZX Spectrum",
    "Commodore 64",
    "Apple II",
    "MSX",
    "Teletext 8",
    "RGB 3-3-2",
    "Web Safe 216",
    "Levels per Channel",
    "Custom Colors",
    "From Layer",
];
const LEVELS: u32 = 13;
const CUSTOM: u32 = 14;
const FROM_LAYER: u32 = 15;

const CUSTOM_IDS: [&str; 8] =
    ["rdCustom/color1", "rdCustom/color2", "rdCustom/color3", "rdCustom/color4", "rdCustom/color5", "rdCustom/color6", "rdCustom/color7", "rdCustom/color8"];

pub fn specs() -> Vec<EffectSpec> {
    let mut params = vec![
        p("method", "Dither Method", Value::Enum(7), popup(METHODS)),
        p("palette", "Palette", Value::Enum(5), popup(PALETTES)),
        p("pixelSize", "Pixel Size", num(1.0), slider(1.0, 256.0, 1.0, 16.0, 1)),
        p("amount", "Dither Amount", num(100.0), slider(0.0, 200.0, 0.0, 200.0, 1)),
        p("threshold", "Threshold", num(0.0), slider(-100.0, 100.0, -100.0, 100.0, 1)),
        p("serpentine", "Serpentine Scan", Value::Bool(true), ParamUi::Checkbox),
        p("grayscale", "Grayscale First", Value::Bool(false), ParamUi::Checkbox),
        p("perceptual", "Perceptual Matching", Value::Bool(true), ParamUi::Checkbox),
        p("levels", "Levels per Channel", num(4.0), slider(2.0, 64.0, 2.0, 16.0, 0)),
        p("rdCustom/count", "Number of Colors", num(4.0), slider(2.0, 8.0, 2.0, 8.0, 0)),
    ];
    let defaults = [
        (0.08, 0.06, 0.12),
        (0.36, 0.22, 0.48),
        (0.85, 0.36, 0.42),
        (0.98, 0.86, 0.62),
        (0.2, 0.55, 0.6),
        (0.55, 0.8, 0.45),
        (0.95, 0.6, 0.2),
        (1.0, 1.0, 1.0),
    ];
    let names = ["Color 1", "Color 2", "Color 3", "Color 4", "Color 5", "Color 6", "Color 7", "Color 8"];
    for ((id, name), (r, g, b)) in CUSTOM_IDS.iter().zip(names).zip(defaults) {
        params.push(p(id, name, col(r, g, b), ParamUi::Color));
    }
    params.extend([
        p("rdLayer/layer", "Palette Layer", Value::Layer(None), ParamUi::Layer),
        p("rdLayer/colors", "Number of Colors", num(8.0), slider(2.0, 64.0, 2.0, 32.0, 0)),
        p("blend", "Blend With Original", num(0.0), slider(0.0, 100.0, 0.0, 100.0, 1)),
    ]);
    vec![EffectSpec { id: ID, name: "Retro Dither", category: "Stylize", params, render: retro_dither, gpu: false, float: true }]
}

// ------------------------------------------------------------------ palettes

/// sRGB hex triplets.
const fn rgb(h: u32) -> [f32; 3] {
    [((h >> 16) & 0xff) as f32 / 255.0, ((h >> 8) & 0xff) as f32 / 255.0, (h & 0xff) as f32 / 255.0]
}

const GAME_BOY: [u32; 4] = [0x0f380f, 0x306230, 0x8bac0f, 0x9bbc0f];
const CGA_4: [u32; 4] = [0x000000, 0x55ffff, 0xff55ff, 0xffffff];
const CGA_16: [u32; 16] = [
    0x000000, 0x0000aa, 0x00aa00, 0x00aaaa, 0xaa0000, 0xaa00aa, 0xaa5500, 0xaaaaaa, 0x555555, 0x5555ff, 0x55ff55, 0x55ffff, 0xff5555, 0xff55ff, 0xffff55,
    0xffffff,
];
const ZX: [u32; 15] =
    [0x000000, 0x0000d7, 0xd70000, 0xd700d7, 0x00d700, 0x00d7d7, 0xd7d700, 0xd7d7d7, 0x0000ff, 0xff0000, 0xff00ff, 0x00ff00, 0x00ffff, 0xffff00, 0xffffff];
const C64: [u32; 16] = [
    0x000000, 0xffffff, 0x68372b, 0x70a4b2, 0x6f3d86, 0x588d43, 0x352879, 0xb8c76f, 0x6f4f25, 0x433900, 0x9a6759, 0x444444, 0x6c6c6c, 0x9ad284, 0x6c5eb5,
    0x959595,
];
const APPLE2: [u32; 15] =
    [0x000000, 0x8a2140, 0x3c22a5, 0xc847e4, 0x07653e, 0x7b7e80, 0x308fe3, 0xb9a9fd, 0x3b5107, 0xc77028, 0xf39ac2, 0x2fb81f, 0xb9d060, 0x6ee1c0, 0xffffff];
const MSX: [u32; 15] =
    [0x000000, 0x21c842, 0x5edc78, 0x5455ed, 0x7d76fc, 0xd4524d, 0x42ebf5, 0xfc5554, 0xff7978, 0xd4c154, 0xe6ce80, 0x21b03b, 0xc95bba, 0xcccccc, 0xffffff];
const TELETEXT: [u32; 8] = [0x000000, 0xff0000, 0x00ff00, 0xffff00, 0x0000ff, 0xff00ff, 0x00ffff, 0xffffff];

/// A palette: a list of colours, or so many evenly spaced levels per channel.
#[derive(Clone, Debug, PartialEq)]
enum Palette {
    List(Vec<[f32; 3]>),
    Levels([u32; 3]),
}

impl Palette {
    fn grays(n: u32) -> Palette {
        let n = n.max(2);
        Palette::List((0..n).map(|i| [i as f32 / (n - 1) as f32; 3]).collect())
    }

    /// Typical distance (largest channel difference) between neighbouring colours: how far
    /// ordered dithering spreads a value.
    fn spacing(&self) -> f32 {
        match self {
            Palette::Levels(l) => l.iter().map(|&n| 1.0 / (n.max(2) - 1) as f32).fold(0.0, f32::max),
            Palette::List(c) if c.len() > 1 => {
                let near: f32 = c
                    .iter()
                    .enumerate()
                    .map(|(i, a)| {
                        c.iter()
                            .enumerate()
                            .filter(|(j, _)| *j != i)
                            .map(|(_, b)| (0..3).map(|k| (a[k] - b[k]).abs()).fold(0.0, f32::max))
                            .fold(f32::MAX, f32::min)
                    })
                    .sum();
                (near / c.len() as f32).clamp(1.0 / 255.0, 1.0)
            }
            Palette::List(_) => 1.0,
        }
    }

    /// The palette colour nearest `c` (weighted squared distance).
    fn nearest(&self, c: [f32; 3], w: [f32; 3]) -> [f32; 3] {
        match self {
            Palette::Levels(l) => [0, 1, 2].map(|i| {
                let n = (l[i].max(2) - 1) as f32;
                (c[i].clamp(0.0, 1.0) * n).round() / n
            }),
            Palette::List(list) => {
                let mut best = [0.0; 3];
                let mut bd = f32::MAX;
                for &q in list {
                    let d = dist2(c, q, w);
                    if d < bd {
                        bd = d;
                        best = q;
                    }
                }
                best
            }
        }
    }
}

fn dist2(a: [f32; 3], b: [f32; 3], w: [f32; 3]) -> f32 {
    (0..3).map(|i| w[i] * (a[i] - b[i]) * (a[i] - b[i])).sum()
}

/// Median cut of `samples` into at most `n` colours (each box's mean), deterministic.
fn median_cut(samples: Vec<[f32; 3]>, n: usize) -> Vec<[f32; 3]> {
    if samples.is_empty() {
        return vec![[0.0; 3], [1.0; 3]];
    }
    let mut boxes: Vec<Vec<[f32; 3]>> = vec![samples];
    while boxes.len() < n {
        // Split the box with the widest channel range.
        let widest = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() > 1)
            .map(|(i, b)| {
                let (axis, range) = (0..3)
                    .map(|c| {
                        let (lo, hi) = b.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p[c]), hi.max(p[c])));
                        (c, hi - lo)
                    })
                    .fold((0, -1.0), |a, x| if x.1 > a.1 { x } else { a });
                (i, axis, range)
            })
            .fold(None, |best: Option<(usize, usize, f32)>, x| match best {
                Some(b) if b.2 >= x.2 => Some(b),
                _ => Some(x),
            });
        let Some((i, axis, range)) = widest else { break };
        if range <= 0.0 {
            break;
        }
        let mut b = boxes.swap_remove(i);
        b.sort_by(|p, q| p[axis].total_cmp(&q[axis]));
        let upper = b.split_off(b.len() / 2);
        boxes.push(b);
        boxes.push(upper);
    }
    let mut out: Vec<[f32; 3]> = boxes
        .iter()
        .filter(|b| !b.is_empty())
        .map(|b| {
            let s = b.iter().fold([0.0f32; 3], |a, p| [a[0] + p[0], a[1] + p[1], a[2] + p[2]]);
            s.map(|v| v / b.len() as f32)
        })
        .collect();
    out.sort_by(|a, b| luminance(a[0], a[1], a[2]).total_cmp(&luminance(b[0], b[1], b[2])));
    out
}

/// Most samples a From Layer palette reads (evenly strided).
const MAX_SAMPLES: usize = 65_536;

fn palette(ctx: &EffectCtx, b: &Buf, enc: &dyn Fn(f32) -> f32) -> Palette {
    let list = |hex: &[u32]| Palette::List(hex.iter().map(|&h| rgb(h)).collect());
    match ctx.params.e("palette") {
        0 => Palette::grays(2),
        1 => Palette::grays(4),
        2 => Palette::grays(16),
        3 => list(&GAME_BOY),
        4 => list(&CGA_4),
        6 => list(&ZX),
        7 => list(&C64),
        8 => list(&APPLE2),
        9 => list(&MSX),
        10 => list(&TELETEXT),
        11 => Palette::Levels([8, 8, 4]),
        12 => Palette::Levels([6, 6, 6]),
        LEVELS => {
            let n = ctx.params.f("levels");
            let n = if n.is_finite() { n.round().clamp(2.0, 64.0) as u32 } else { 4 };
            Palette::Levels([n; 3])
        }
        CUSTOM => {
            let n = ctx.params.f("rdCustom/count");
            let n = if n.is_finite() { n.round().clamp(2.0, 8.0) as usize } else { 4 };
            Palette::List(
                CUSTOM_IDS
                    .iter()
                    .take(n)
                    .map(|id| {
                        let c = ctx.params.color(id);
                        [c[0], c[1], c[2]].map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 })
                    })
                    .collect(),
            )
        }
        FROM_LAYER => {
            let n = ctx.params.f("rdLayer/colors");
            let n = if n.is_finite() { n.round().clamp(2.0, 64.0) as usize } else { 8 };
            // The chosen layer, else the layer itself.
            let other = ctx.layer_param("rdLayer/layer", false);
            let img = other.as_ref().map_or(&b.img, |o| &o.buf.img);
            let step = (img.data.len() / MAX_SAMPLES).max(1);
            let samples: Vec<[f32; 3]> = img
                .data
                .iter()
                .step_by(step)
                .filter(|p| p[3] > 0.5)
                .map(|&p| {
                    let (c, _) = unpremul(p);
                    c.map(|v| if v.is_finite() { enc(v).clamp(0.0, 1.0) } else { 0.0 })
                })
                .collect();
            Palette::List(median_cut(samples, n))
        }
        _ => list(&CGA_16),
    }
}

// ------------------------------------------------------------------ threshold maps

/// Bayer matrix of size 2^`order` (values 0..n²−1), row-major.
fn bayer(order: u32) -> Vec<u32> {
    let mut m = vec![0u32];
    let mut n = 1usize;
    for _ in 0..order {
        let nn = n * 2;
        let mut next = vec![0u32; nn * nn];
        for y in 0..nn {
            for x in 0..nn {
                let base = m.get((y % n) * n + (x % n)).copied().unwrap_or(0) * 4;
                let q = match (x / n, y / n) {
                    (0, 0) => 0,
                    (1, 1) => 1,
                    (1, 0) => 2,
                    _ => 3,
                };
                if let Some(v) = next.get_mut(y * nn + x) {
                    *v = base + q;
                }
            }
        }
        m = next;
        n = nn;
    }
    m
}

/// A 4×4 clustered-dot (halftone) order: dots grow from the cell's centre.
const CLUSTER: [u32; 16] = [12, 5, 6, 13, 4, 0, 1, 7, 11, 3, 2, 8, 15, 10, 9, 14];

fn hash2(x: i64, y: i64, seed: u64) -> f32 {
    let mut z = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ seed;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 40) as f32 / (1u64 << 24) as f32
}

/// The ordered threshold of cell (`x`, `y`) in −0.5..0.5 for ordered method `m`.
struct Ordered {
    method: u32,
    bayer: Vec<u32>,
    n: i64,
    seed: u64,
}

impl Ordered {
    fn new(method: u32, seed: u64) -> Ordered {
        let order = match method {
            1 => 1,
            2 => 2,
            3 => 3,
            _ => 0,
        };
        let n = 1i64 << order;
        Ordered { method, bayer: bayer(order), n, seed }
    }

    fn at(&self, x: i64, y: i64) -> f32 {
        match self.method {
            1..=3 => {
                let (i, j) = (x.rem_euclid(self.n), y.rem_euclid(self.n));
                let v = self.bayer.get((j * self.n + i) as usize).copied().unwrap_or(0);
                (v as f32 + 0.5) / (self.n * self.n) as f32 - 0.5
            }
            4 => {
                let v = CLUSTER.get((y.rem_euclid(4) * 4 + x.rem_euclid(4)) as usize).copied().unwrap_or(0);
                (v as f32 + 0.5) / 16.0 - 0.5
            }
            // Interleaved gradient noise (a published screen-space noise function).
            5 => {
                let f = |v: f64| v - v.floor();
                (f(52.982_918_9 * f(0.067_110_56 * x as f64 + 0.005_837_15 * y as f64)) - 0.5) as f32
            }
            6 => hash2(x, y, self.seed) - 0.5,
            _ => 0.0,
        }
    }
}

// ------------------------------------------------------------------ error diffusion kernels

/// (dx, dy, weight) of the cells after the current one, and the divisor.
fn kernel(method: u32) -> (&'static [(i32, i32, f32)], f32) {
    match method {
        7 => (&[(1, 0, 7.0), (-1, 1, 3.0), (0, 1, 5.0), (1, 1, 1.0)], 16.0),
        8 => (
            &[
                (1, 0, 7.0),
                (2, 0, 5.0),
                (-2, 1, 3.0),
                (-1, 1, 5.0),
                (0, 1, 7.0),
                (1, 1, 5.0),
                (2, 1, 3.0),
                (-2, 2, 1.0),
                (-1, 2, 3.0),
                (0, 2, 5.0),
                (1, 2, 3.0),
                (2, 2, 1.0),
            ],
            48.0,
        ),
        9 => (
            &[
                (1, 0, 8.0),
                (2, 0, 4.0),
                (-2, 1, 2.0),
                (-1, 1, 4.0),
                (0, 1, 8.0),
                (1, 1, 4.0),
                (2, 1, 2.0),
                (-2, 2, 1.0),
                (-1, 2, 2.0),
                (0, 2, 4.0),
                (1, 2, 2.0),
                (2, 2, 1.0),
            ],
            42.0,
        ),
        // Atkinson spreads only 6/8 of the error (the rest is dropped).
        10 => (&[(1, 0, 1.0), (2, 0, 1.0), (-1, 1, 1.0), (0, 1, 1.0), (1, 1, 1.0), (0, 2, 1.0)], 8.0),
        11 => (&[(1, 0, 8.0), (2, 0, 4.0), (-2, 1, 2.0), (-1, 1, 4.0), (0, 1, 8.0), (1, 1, 4.0), (2, 1, 2.0)], 32.0),
        12 => (&[(1, 0, 5.0), (2, 0, 3.0), (-2, 1, 2.0), (-1, 1, 4.0), (0, 1, 5.0), (1, 1, 4.0), (2, 1, 2.0), (-1, 2, 2.0), (0, 2, 3.0), (1, 2, 2.0)], 32.0),
        13 => (&[(1, 0, 4.0), (2, 0, 3.0), (-2, 1, 1.0), (-1, 1, 2.0), (0, 1, 3.0), (1, 1, 2.0), (2, 1, 1.0)], 16.0),
        14 => (&[(1, 0, 2.0), (-1, 1, 1.0), (0, 1, 1.0)], 4.0),
        _ => (
            &[
                (2, 0, 32.0),
                (-3, 1, 12.0),
                (-1, 1, 26.0),
                (1, 1, 30.0),
                (3, 1, 16.0),
                (-2, 2, 12.0),
                (0, 2, 26.0),
                (2, 2, 12.0),
                (-3, 3, 5.0),
                (-1, 3, 12.0),
                (1, 3, 12.0),
                (3, 3, 5.0),
            ],
            200.0,
        ),
    }
}

// ------------------------------------------------------------------ the effect

/// Largest grid of cells the effect dithers (cells above it get bigger).
const MAX_CELLS: f64 = 16_777_216.0;

fn retro_dither(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let (w, h) = (b.img.width as usize, b.img.height as usize);
    if w == 0 || h == 0 || b.img.data.len() != w * h {
        return b;
    }
    let f = |id: &str, d: f64| {
        let v = ctx.params.f(id);
        if v.is_finite() { v } else { d }
    };
    let mix = (1.0 - f("blend", 0.0) / 100.0).clamp(0.0, 1.0) as f32;
    if mix <= 0.0 {
        return b;
    }
    // Match on display-encoded values.
    let linear = ctx.env.working_linear;
    let enc = move |v: f32| if linear { linear_to_srgb(v) } else { v };
    let dec = move |v: f32| if linear { srgb_to_linear(v) } else { v };
    let pal = palette(ctx, &b, &enc);
    let method = ctx.params.e("method").min(15);
    let amount = (f("amount", 100.0) / 100.0).clamp(0.0, 2.0) as f32;
    let bias = (f("threshold", 0.0) / 200.0).clamp(-0.5, 0.5) as f32;
    let gray = ctx.params.b("grayscale");
    let wts = if ctx.params.b("perceptual") { [0.299, 0.587, 0.114] } else { [1.0; 3] };

    // The cell grid, aligned to the layer origin.
    let mut cell = (f("pixelSize", 1.0).clamp(1.0, 256.0) * b.scale).max(1.0);
    while (w as f64 / cell).ceil() * (h as f64 / cell).ceil() > MAX_CELLS {
        cell *= 2.0;
    }
    let ox = b.offset[0];
    let oy = b.offset[1];
    let gx = |x: usize| ((x as f64 - ox) / cell).floor() as i64;
    let gy = |y: usize| ((y as f64 - oy) / cell).floor() as i64;
    let (gx0, gy0) = (gx(0), gy(0));
    let (gw, gh) = ((gx(w - 1) - gx0 + 1).max(1) as usize, (gy(h - 1) - gy0 + 1).max(1) as usize);

    // Average each cell (straight colour weighted by alpha, and mean alpha).
    let mut acc = vec![[0.0f32; 5]; gw * gh];
    for y in 0..h {
        let cy = (gy(y) - gy0) as usize;
        for x in 0..w {
            let cx = (gx(x) - gx0) as usize;
            let (Some(px), Some(a)) = (b.img.data.get(y * w + x), acc.get_mut(cy * gw + cx)) else { continue };
            let px = px.map(|v| if v.is_finite() { v } else { 0.0 });
            for c in 0..4 {
                a[c] += px[c];
            }
            a[4] += 1.0;
        }
    }
    let mut cells: Vec<([f32; 3], f32)> = acc
        .iter()
        .map(|a| {
            let n = a[4].max(1.0);
            let alpha = a[3] / n;
            let (c, _) = unpremul([a[0] / n, a[1] / n, a[2] / n, alpha]);
            let mut c = c.map(|v| enc(v).clamp(0.0, 1.0));
            if gray {
                c = [luminance(c[0], c[1], c[2]); 3];
            }
            (c.map(|v| (v + bias).clamp(0.0, 1.0)), alpha)
        })
        .collect();

    let spread = pal.spacing() * amount;
    if method < DIFFUSION {
        let ord = Ordered::new(method, u64::from(ctx.seed));
        for (i, (c, a)) in cells.iter_mut().enumerate() {
            if *a <= 0.0 {
                continue;
            }
            let t = ord.at(gx0 + (i % gw) as i64, gy0 + (i / gw) as i64) * spread;
            *c = pal.nearest(c.map(|v| v + t), wts);
        }
    } else {
        let (k, div) = kernel(method);
        let serp = ctx.params.b("serpentine");
        let mut err = vec![[0.0f32; 3]; gw * gh];
        for y in 0..gh {
            let rev = serp && y % 2 == 1;
            for s in 0..gw {
                let x = if rev { gw - 1 - s } else { s };
                let i = y * gw + x;
                let (Some((c, a)), Some(e)) = (cells.get(i).copied(), err.get(i).copied()) else { continue };
                if a <= 0.0 {
                    continue;
                }
                // Clamp the carried error so runaway values can't build up.
                let want = [0, 1, 2].map(|j| (c[j] + e[j]).clamp(-0.5, 1.5));
                let got = pal.nearest(want, wts);
                if let Some(cell) = cells.get_mut(i) {
                    cell.0 = got;
                }
                let q = [0, 1, 2].map(|j| (want[j] - got[j]) * amount);
                for &(dx, dy, wt) in k {
                    let dx = if rev { -dx } else { dx };
                    let (nx, ny) = (x as i64 + i64::from(dx), y as i64 + i64::from(dy));
                    if nx < 0 || ny < 0 || nx >= gw as i64 || ny >= gh as i64 {
                        continue;
                    }
                    if let Some(t) = err.get_mut(ny as usize * gw + nx as usize) {
                        for j in 0..3 {
                            t[j] += q[j] * wt / div;
                        }
                    }
                }
            }
        }
    }
    // Draw the cells back.
    for y in 0..h {
        let cy = (gy(y) - gy0) as usize;
        for x in 0..w {
            let cx = (gx(x) - gx0) as usize;
            let (Some(&(c, _)), Some(px)) = (cells.get(cy * gw + cx), b.img.data.get_mut(y * w + x)) else { continue };
            let (_, a) = unpremul(*px);
            if a <= 0.0 {
                continue;
            }
            let out: Px = premul(c.map(dec), a);
            *px = if mix >= 1.0 { out } else { [0, 1, 2, 3].map(|i| px[i] + (out[i] - px[i]) * mix) };
        }
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Params;
    use effectcraft_raster::Image;

    fn at(img: &Image, x: u32, y: u32) -> Px {
        img.get(i64::from(x), i64::from(y))
    }

    fn run(vals: &[(&str, Value)], img: Image) -> Image {
        let s = crate::find(ID).unwrap();
        let mut params = Params { values: s.params.iter().map(|p| (p.id.to_string(), p.default.clone())).collect() };
        for (k, v) in vals {
            params.values.insert(k.to_string(), v.clone());
        }
        let ctx =
            EffectCtx { params: &params, time: 0.0, layer_size: [img.width as f64, img.height as f64], seed: 1, adjustment: false, env: Default::default() };
        crate::apply(s, &ctx, Buf { img, offset: [0.0, 0.0], scale: 1.0 }).img
    }

    fn flat(w: u32, h: u32, c: [f32; 3]) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.set(x, y, [c[0], c[1], c[2], 1.0]);
            }
        }
        img
    }

    fn ramp(w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = x as f32 / (w - 1) as f32;
                img.set(x, y, [v, (v * 0.7 + 0.1), 1.0 - v, 1.0]);
            }
        }
        img
    }

    fn colors(img: &Image) -> std::collections::BTreeSet<[u32; 3]> {
        img.data.iter().map(|p| [p[0], p[1], p[2]].map(|v| (v * 255.0).round() as u32)).collect()
    }

    #[test]
    fn bayer_matrices_hold_every_rank_once() {
        for order in 1..=3 {
            let mut m = bayer(order);
            m.sort_unstable();
            assert_eq!(m, (0..(1u32 << (2 * order))).collect::<Vec<_>>());
        }
        assert_eq!(bayer(1), [0, 2, 3, 1]);
        let mut c = CLUSTER.to_vec();
        c.sort_unstable();
        assert_eq!(c, (0..16).collect::<Vec<u32>>());
    }

    #[test]
    fn kernels_spread_what_they_say() {
        for m in DIFFUSION..16 {
            let (k, div) = kernel(m);
            let sum: f32 = k.iter().map(|e| e.2).sum();
            let want = if m == 10 { 6.0 } else { div };
            assert!((sum - want).abs() < 1e-4, "method {m}: {sum} of {div}");
            assert!(k.iter().all(|&(dx, dy, _)| dy > 0 || dx > 0), "method {m} only pushes forward");
        }
    }

    #[test]
    fn every_method_uses_only_palette_colours() {
        let img = ramp(48, 12);
        for m in 0..16 {
            let out = run(&[("method", Value::Enum(m)), ("palette", Value::Enum(3))], img.clone());
            let want: std::collections::BTreeSet<[u32; 3]> = GAME_BOY.iter().map(|&h| rgb(h).map(|v| (v * 255.0).round() as u32)).collect();
            assert!(colors(&out).is_subset(&want), "method {m}: {:?}", colors(&out));
        }
    }

    #[test]
    fn a_mid_grey_dithers_to_half_black_half_white() {
        for m in [3u32, 7, 9, 12] {
            let out = run(&[("method", Value::Enum(m)), ("palette", Value::Enum(0))], flat(32, 32, [0.5; 3]));
            let white = out.data.iter().filter(|p| p[0] > 0.5).count() as f32 / out.data.len() as f32;
            assert!((white - 0.5).abs() < 0.06, "method {m}: {white}");
        }
        // None just thresholds; Threshold biases it.
        let none = run(&[("method", Value::Enum(0)), ("palette", Value::Enum(0))], flat(8, 8, [0.45; 3]));
        assert!(none.data.iter().all(|p| p[0] == 0.0));
        let biased = run(&[("method", Value::Enum(0)), ("palette", Value::Enum(0)), ("threshold", num(20.0))], flat(8, 8, [0.45; 3]));
        assert!(biased.data.iter().all(|p| p[0] == 1.0));
    }

    #[test]
    fn atkinson_keeps_its_dropped_error_and_dither_amount_zero_is_plain_quantising() {
        // Atkinson loses 1/4 of the error, so light greys turn pure white more readily.
        let atk = run(&[("method", Value::Enum(10)), ("palette", Value::Enum(0))], flat(32, 32, [0.85; 3]));
        let fs = run(&[("method", Value::Enum(7)), ("palette", Value::Enum(0))], flat(32, 32, [0.85; 3]));
        let whites = |i: &Image| i.data.iter().filter(|p| p[0] > 0.5).count();
        assert!(whites(&atk) > whites(&fs));
        let off = run(&[("method", Value::Enum(7)), ("palette", Value::Enum(0)), ("amount", num(0.0))], flat(8, 8, [0.6; 3]));
        assert!(off.data.iter().all(|p| p[0] == 1.0));
    }

    #[test]
    fn pixel_size_makes_blocks_aligned_to_the_layer() {
        let out = run(&[("pixelSize", num(4.0)), ("palette", Value::Enum(5))], ramp(32, 8));
        for by in 0..2 {
            for bx in 0..8 {
                let first = at(&out, bx * 4, by * 4);
                for y in 0..4 {
                    for x in 0..4 {
                        assert_eq!(at(&out, bx * 4 + x, by * 4 + y), first, "block {bx},{by}");
                    }
                }
            }
        }
    }

    #[test]
    fn levels_palettes_quantise_each_channel() {
        let out = run(&[("palette", Value::Enum(LEVELS)), ("levels", num(3.0)), ("method", Value::Enum(0))], ramp(40, 4));
        for p in &out.data {
            for v in &p[..3] {
                assert!([0.0, 0.5, 1.0].iter().any(|l| (v - l).abs() < 1e-6), "{v}");
            }
        }
        let web = run(&[("palette", Value::Enum(12))], ramp(64, 8));
        assert!(colors(&web).iter().all(|c| c.iter().all(|v| v % 51 == 0)));
    }

    #[test]
    fn custom_and_layer_palettes() {
        let custom = run(&[("palette", Value::Enum(CUSTOM)), ("rdCustom/count", num(2.0))], ramp(40, 4));
        assert!(colors(&custom).len() <= 2);
        // From Layer without a layer takes the image's own colours.
        let mut img = flat(16, 16, [1.0, 0.0, 0.0]);
        for y in 8..16 {
            for x in 0..16 {
                img.set(x, y, [0.0, 0.0, 1.0, 1.0]);
            }
        }
        let out = run(&[("palette", Value::Enum(FROM_LAYER)), ("rdLayer/colors", num(2.0))], img.clone());
        assert_eq!(out.data, img.data, "a two-colour picture keeps its two colours");
        let one = median_cut(vec![[0.2; 3]; 10], 4);
        assert_eq!(one.len(), 1);
        assert!(one[0].iter().all(|v| (v - 0.2).abs() < 1e-6));
    }

    #[test]
    fn alpha_is_kept_and_transparent_pixels_stay_empty() {
        let mut img = ramp(16, 4);
        img.set(3, 1, [0.0; 4]);
        img.set(5, 2, [0.25, 0.25, 0.25, 0.5]);
        let out = run(&[("palette", Value::Enum(0))], img);
        assert_eq!(at(&out, 3, 1), [0.0; 4]);
        let p = at(&out, 5, 2);
        assert_eq!(p[3], 0.5);
        assert!(p[0] == 0.0 || (p[0] - 0.5).abs() < 1e-6, "{p:?}");
    }

    #[test]
    fn blend_and_hostile_values() {
        let img = ramp(20, 6);
        assert_eq!(run(&[("blend", num(100.0))], img.clone()).data, img.data);
        let mut bad = ramp(9, 5);
        bad.set(2, 2, [f32::NAN, f32::INFINITY, -4.0, 1.0]);
        for vals in [
            vec![("pixelSize", num(f64::NAN)), ("amount", num(f64::INFINITY)), ("threshold", num(-1e9))],
            vec![("pixelSize", num(1e12)), ("method", Value::Enum(99)), ("palette", Value::Enum(99))],
            vec![("levels", num(f64::NAN)), ("palette", Value::Enum(LEVELS)), ("rdCustom/count", num(-3.0))],
        ] {
            let out = run(&vals, bad.clone());
            assert_eq!((out.width, out.height), (9, 5));
            assert!(out.data.iter().all(|p| p.iter().all(|v| v.is_finite())));
        }
        assert!(run(&[], Image::new(0, 0)).data.is_empty());
    }
}
