//! Pixel Sort: an original EffectCraft effect (Stylize), not an After Effects one. It makes the
//! "pixel sorting" glitch look: along parallel lines at an angle, runs of pixels whose span key
//! lies between two thresholds are sorted by a sort key, so bright (or dark) areas smear into
//! ordered streaks while the rest of the image stays put.
//!
//! Written from the general idea of pixel sorting as an image technique and the published feature
//! lists of pixel-sorting plug-ins (sort modes, thresholds, angle, span length and random length,
//! feathered span edges, gradient spans, a matte that limits where sorting happens); the
//! algorithm, parameters and maths are our own.
//!
//! * **Lines.** For a direction `d`, every pixel belongs to exactly one line: when the line is
//!   closer to horizontal, line `k` visits `(x, k + round(x·dy/dx))` for every column (and the
//!   same with rows and columns swapped when it is closer to vertical), walked along `d`. The
//!   lines partition the buffer, so each pixel is read and written once.
//! * **Spans.** A pixel can be sorted when its span key is within [Lower, Upper] Threshold
//!   (or outside with Invert Spans), it isn't transparent and the Constraint Layer (if any)
//!   covers it. Runs of such pixels are cut into spans no longer than Max Span Length, each one
//!   shortened at random by up to Random Length.
//! * **Sorting.** Each span is sorted by the sort key (stable, ascending along the direction;
//!   Reverse Order descends), or shuffled (Random). Whole premultiplied pixels move, so colour
//!   and alpha stay together. Gradient fill replaces a sorted span by a ramp from its first to
//!   its last pixel.
//! * **Blending.** Edge Feather fades the sorted result into the original towards both ends of a
//!   span; the Constraint Layer's value and Blend With Original scale it too.
//! * **Stretch and Mirror.** Stretch smears the span's first pixel over the whole span instead
//!   of sorting it; Mirror Sort lays the sorted run out up to the span's middle and back down,
//!   so both ends of a streak match (no hard seam when spans grow and shrink in animation).
//! * **Masks.** Constrain to Masks only sorts inside the layer's closed masks (outside an
//!   inverted one).
//! * **Noise** adds smooth value noise, stretched along the sort direction, to the span key
//!   (ragged span ends), the sort key (striated streaks) or both; Evolution animates it.
//! * **Floating Particles** sends short glowing streaks out of the bright end of sorted spans,
//!   along the direction, at a given speed and over a given distance, fading as they go.

use effectcraft_color::{luminance, rgb_to_hsl};
use effectcraft_keyframe::Value;
use effectcraft_project::ParamUi;
use effectcraft_raster::{Image, Px};
use rayon::prelude::*;

use crate::util::unpremul;
use crate::{Buf, EffectCtx, EffectSpec, num, p, popup, slider};

pub const ID: &str = "ec.stylize.pixelsort";

/// Keys a span can be found and sorted by (`Sort By` adds Random).
const KEYS: &[&str] = &["Luminance", "Hue", "Saturation", "Lightness", "Red", "Green", "Blue", "Alpha"];
const SORT_BY: &[&str] = &["Luminance", "Hue", "Saturation", "Lightness", "Red", "Green", "Blue", "Alpha", "Random"];
const RANDOM: u32 = 8;
const FILLS: &[&str] = &["Sorted Pixels", "Gradient", "Stretch"];
const NOISE_TARGETS: &[&str] = &["Spans and Sort Key", "Spans Only", "Sort Key Only"];

/// Twirl-down groups (id, display name).
pub const GROUPS: &[(&str, &str)] = &[("psNoise", "Noise"), ("psParticles", "Floating Particles")];
const CONSTRAINT_CHANNELS: &[&str] = &["Luminance", "Alpha"];

/// Longest span length, in pixels, the parameters accept (a 16K line).
const MAX_LENGTH: f64 = 16_384.0;

pub fn specs() -> Vec<EffectSpec> {
    vec![EffectSpec {
        id: ID,
        name: "Pixel Sort",
        category: "Stylize",
        params: vec![
            p("sortBy", "Sort By", Value::Enum(0), popup(SORT_BY)),
            p("reverse", "Reverse Order", Value::Bool(false), ParamUi::Checkbox),
            p("direction", "Direction", num(90.0), ParamUi::Angle),
            p("spanKey", "Find Spans By", Value::Enum(0), popup(KEYS)),
            p("lowerThreshold", "Lower Threshold", num(25.0), slider(0.0, 100.0, 0.0, 100.0, 1)),
            p("upperThreshold", "Upper Threshold", num(100.0), slider(0.0, 100.0, 0.0, 100.0, 1)),
            p("invertSpans", "Invert Spans", Value::Bool(false), ParamUi::Checkbox),
            p("minLength", "Min Span Length", num(0.0), slider(0.0, MAX_LENGTH, 0.0, 200.0, 0)),
            p("maxLength", "Max Span Length", num(0.0), slider(0.0, MAX_LENGTH, 0.0, 1000.0, 0)),
            p("randomLength", "Random Length", num(0.0), slider(0.0, 100.0, 0.0, 100.0, 1)),
            p("randomSeed", "Random Seed", num(0.0), slider(0.0, 10_000.0, 0.0, 1000.0, 0)),
            p("fill", "Span Fill", Value::Enum(0), popup(FILLS)),
            p("mirror", "Mirror Sort", Value::Bool(false), ParamUi::Checkbox),
            p("feather", "Edge Feather", num(0.0), slider(0.0, 1000.0, 0.0, 100.0, 1)),
            p("constraintLayer", "Constraint Layer", Value::Layer(None), ParamUi::Layer),
            p("constraintChannel", "Constraint Channel", Value::Enum(0), popup(CONSTRAINT_CHANNELS)),
            p("invertConstraint", "Invert Constraint", Value::Bool(false), ParamUi::Checkbox),
            p("constrainToMasks", "Constrain to Masks", Value::Bool(false), ParamUi::Checkbox),
            p("psNoise/amount", "Noise Amount", num(0.0), slider(0.0, 100.0, 0.0, 100.0, 1)),
            p("psNoise/target", "Noise Affects", Value::Enum(0), popup(NOISE_TARGETS)),
            p("psNoise/scale", "Noise Scale", num(40.0), slider(1.0, 4000.0, 2.0, 400.0, 1)),
            p("psNoise/stretch", "Stretch Along Direction", num(4.0), slider(1.0, 100.0, 1.0, 20.0, 1)),
            p("psNoise/evolution", "Evolution", num(0.0), slider(-100_000.0, 100_000.0, 0.0, 10.0, 2)),
            p("psParticles/amount", "Particle Amount", num(0.0), slider(0.0, 100.0, 0.0, 100.0, 1)),
            p("psParticles/length", "Particle Length", num(12.0), slider(1.0, 2000.0, 1.0, 100.0, 1)),
            p("psParticles/speed", "Speed (px/s)", num(120.0), slider(-100_000.0, 100_000.0, -1000.0, 1000.0, 1)),
            p("psParticles/distance", "Travel Distance", num(150.0), slider(1.0, MAX_LENGTH, 1.0, 1000.0, 1)),
            p("psParticles/opacity", "Particle Opacity", num(100.0), slider(0.0, 100.0, 0.0, 100.0, 1)),
            p("psParticles/add", "Add Particles", Value::Bool(true), ParamUi::Checkbox),
            p("blend", "Blend With Original", num(0.0), slider(0.0, 100.0, 0.0, 100.0, 1)),
        ],
        render: pixel_sort,
        gpu: false,
        float: true,
    }]
}

/// The evaluated parameters, lengths in buffer pixels.
#[derive(Clone, Copy, Debug)]
struct Settings {
    sort_by: u32,
    reverse: bool,
    span_key: u32,
    lower: f32,
    upper: f32,
    invert: bool,
    min_len: usize,
    max_len: usize,
    random_len: f64,
    seed: u64,
    /// 0 sorted pixels, 1 gradient, 2 stretch.
    fill: u32,
    mirror: bool,
    feather: f64,
    amount: f32,
    noise: Noise,
    particles: Particles,
}

/// Key noise: amount (key units), where it goes, and its frame along the direction.
#[derive(Clone, Copy, Debug, Default)]
struct Noise {
    amount: f32,
    spans: bool,
    sort: bool,
    /// Buffer pixels per noise cell across / along the lines.
    across: f64,
    along: f64,
    evolution: f32,
    dir: [f64; 2],
    seed: u32,
}

impl Noise {
    /// Noise in −amount..amount at buffer pixel (`x`, `y`).
    fn at(&self, x: usize, y: usize) -> f32 {
        let (x, y) = (x as f64 + 0.5, y as f64 + 0.5);
        let u = (x * -self.dir[1] + y * self.dir[0]) / self.across;
        let v = (x * self.dir[0] + y * self.dir[1]) / self.along;
        let n = crate::noise::fbm(u as f32, v as f32, self.evolution, self.seed, 3.0);
        let n = if n.is_finite() { n } else { 0.5 };
        (n * 2.0 - 1.0) * self.amount
    }
}

/// Floating particles out of the bright end of spans.
#[derive(Clone, Copy, Debug, Default)]
struct Particles {
    /// Chance per span (0 = off).
    amount: f64,
    length: f64,
    /// Buffer pixels the head has travelled at this time (before wrapping).
    travelled: f64,
    distance: f64,
    opacity: f32,
    add: bool,
}

/// A whole number of buffer pixels from a parameter in layer pixels (`0` when not finite).
fn px_len(v: f64, scale: f64) -> usize {
    let n = (v.clamp(0.0, MAX_LENGTH) * scale).round();
    if n.is_finite() { n as usize } else { 0 }
}

fn settings(ctx: &EffectCtx, scale: f64) -> Settings {
    let f = |id: &str| {
        let v = ctx.params.f(id);
        if v.is_finite() { v } else { 0.0 }
    };
    let e = |id: &str| ctx.params.get(id).map(Value::as_enum).unwrap_or(0);
    let b = |id: &str| ctx.params.get(id).is_some_and(Value::as_bool);
    let (lo, hi) = ((f("lowerThreshold") / 100.0) as f32, (f("upperThreshold") / 100.0) as f32);
    Settings {
        sort_by: e("sortBy"),
        reverse: b("reverse"),
        span_key: e("spanKey"),
        lower: lo.min(hi),
        upper: lo.max(hi),
        invert: b("invertSpans"),
        min_len: px_len(f("minLength"), scale),
        max_len: px_len(f("maxLength"), scale),
        random_len: (f("randomLength") / 100.0).clamp(0.0, 1.0),
        seed: (f("randomSeed").clamp(0.0, 1e9) as u64) ^ (u64::from(ctx.seed) << 32),
        fill: e("fill").min(2),
        mirror: b("mirror"),
        feather: (f("feather") * scale).max(0.0),
        amount: (1.0 - f("blend") / 100.0).clamp(0.0, 1.0) as f32,
        noise: Noise::default(),
        particles: Particles::default(),
    }
}

/// Noise and particle settings, which depend on the direction and time.
fn extras(ctx: &EffectCtx, scale: f64, dir: [f64; 2], st: &mut Settings) {
    let f = |id: &str, d: f64| {
        let v = ctx.params.f(id);
        if v.is_finite() { v } else { d }
    };
    let amount = (f("psNoise/amount", 0.0) / 100.0).clamp(0.0, 1.0) as f32;
    if amount > 0.0 {
        let target = ctx.params.e("psNoise/target");
        let cell = (f("psNoise/scale", 40.0).clamp(1.0, 4000.0) * scale).max(1e-3);
        st.noise = Noise {
            amount,
            spans: target != 2,
            sort: target != 1,
            across: cell,
            along: cell * f("psNoise/stretch", 4.0).clamp(1.0, 100.0),
            evolution: f("psNoise/evolution", 0.0).clamp(-1e5, 1e5) as f32,
            dir,
            seed: ctx.seed ^ 0x5053_4E5A,
        };
    }
    let p = (f("psParticles/amount", 0.0) / 100.0).clamp(0.0, 1.0);
    if p > 0.0 {
        let t = if ctx.time.is_finite() { ctx.time.clamp(-1e6, 1e6) } else { 0.0 };
        st.particles = Particles {
            amount: p,
            length: (f("psParticles/length", 12.0).clamp(1.0, 2000.0) * scale).max(1.0),
            travelled: f("psParticles/speed", 120.0).clamp(-1e5, 1e5) * scale * t,
            distance: (f("psParticles/distance", 150.0).clamp(1.0, MAX_LENGTH) * scale).max(1.0),
            opacity: (f("psParticles/opacity", 100.0) / 100.0).clamp(0.0, 1.0) as f32,
            add: ctx.params.b("psParticles/add"),
        };
    }
}

/// Key `k` (an index into [`KEYS`]) of a premultiplied pixel, 0..1 for in-range colours.
fn key(px: Px, k: u32) -> f32 {
    let (c, a) = unpremul(px);
    let v = match k {
        1 => rgb_to_hsl(c[0], c[1], c[2]).0,
        2 => rgb_to_hsl(c[0], c[1], c[2]).1,
        3 => rgb_to_hsl(c[0], c[1], c[2]).2,
        4 => c[0],
        5 => c[1],
        6 => c[2],
        7 => a,
        _ => luminance(c[0], c[1], c[2]),
    };
    if v.is_finite() { v } else { 0.0 }
}

/// SplitMix64 (seeded, deterministic randomness for span lengths and Random order).
fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Uniform 0..1 from a hash.
fn unit(h: u64) -> f64 {
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// The lines along direction `d` (unit vector, y down) through a `w`×`h` grid: how to walk
/// each one. Every pixel lies on exactly one line.
#[derive(Clone, Copy, Debug)]
struct Lines {
    w: usize,
    h: usize,
    /// Closer to horizontal: lines step along x.
    along_x: bool,
    /// Cross-axis offset per step along the line.
    slope: f64,
    /// Walk the major axis backwards (direction points left / up).
    backwards: bool,
    /// Line indices `k_min..=k_max`.
    k_min: i64,
    k_max: i64,
}

impl Lines {
    fn new(w: usize, h: usize, d: [f64; 2]) -> Lines {
        let along_x = d[0].abs() >= d[1].abs();
        let (major, minor) = if along_x { (d[0], d[1]) } else { (d[1], d[0]) };
        let slope = if major.abs() > 1e-12 { (minor / major).clamp(-1.0, 1.0) } else { 0.0 };
        let (n_major, n_minor) = if along_x { (w, h) } else { (h, w) };
        // Offsets round(i·slope) over the major axis lie between these.
        let end = (n_major.saturating_sub(1) as f64 * slope).round() as i64;
        let (o_min, o_max) = (end.min(0), end.max(0));
        Lines { w, h, along_x, slope, backwards: major < 0.0, k_min: -o_max, k_max: n_minor as i64 - 1 - o_min }
    }

    /// Buffer indices of line `k`, in walking order.
    fn indices(&self, k: i64) -> Vec<usize> {
        let (n_major, n_minor) = if self.along_x { (self.w, self.h) } else { (self.h, self.w) };
        let mut v = Vec::with_capacity(n_major);
        for step in 0..n_major {
            let i = if self.backwards { n_major - 1 - step } else { step };
            let j = k + (i as f64 * self.slope).round() as i64;
            if j < 0 || j >= n_minor as i64 {
                continue;
            }
            let (x, y) = if self.along_x { (i, j as usize) } else { (j as usize, i) };
            v.push(y * self.w + x);
        }
        v
    }
}

/// One line's pixels, with what's needed to find and sort its spans.
struct LineData {
    px: Vec<Px>,
    /// The Constraint Layer's (and masks') value per pixel (1 without one).
    weight: Vec<f32>,
    /// Key noise per pixel (empty without noise).
    noise: Vec<f32>,
}

impl LineData {
    fn noise(&self, i: usize) -> f32 {
        self.noise.get(i).copied().unwrap_or(0.0)
    }
}

/// Where spans start and end on a line (`[start, end)` in line positions).
fn spans(line: &LineData, st: &Settings, line_id: i64) -> Vec<(usize, usize)> {
    let selected = |i: usize| -> bool {
        let (Some(&px), Some(&w)) = (line.px.get(i), line.weight.get(i)) else { return false };
        if w < 0.5 || (px[3] <= 1e-6 && st.span_key != 7) {
            return false;
        }
        let v = key(px, st.span_key) + if st.noise.spans { line.noise(i) } else { 0.0 };
        (v >= st.lower && v <= st.upper) != st.invert
    };
    let n = line.px.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if !selected(i) {
            i += 1;
            continue;
        }
        let mut end = i;
        while end < n && selected(end) {
            end += 1;
        }
        // Cut the run into spans no longer than the (randomly shortened) maximum.
        let mut s = i;
        while s < end {
            let base = if st.max_len > 0 { st.max_len } else { end - s };
            let r = unit(mix64(st.seed ^ mix64(line_id as u64 ^ ((s as u64) << 32))));
            let cap = ((base as f64) * (1.0 - st.random_len * r)).round().max(1.0) as usize;
            let e = (s + cap).min(end);
            out.push((s, e));
            s = e;
        }
        i = end;
    }
    out
}

/// The span's pixels in their new order.
fn sorted_span(px: &[Px], noise: &[f32], st: &Settings, salt: u64) -> Vec<Px> {
    if st.fill == 2 {
        // Stretch: the span's first pixel (its last with Reverse Order) smeared over it.
        let Some(&first) = (if st.reverse { px.last() } else { px.first() }) else { return Vec::new() };
        return vec![first; px.len()];
    }
    let mut order: Vec<(f32, usize)> = if st.sort_by == RANDOM {
        px.iter().enumerate().map(|(i, _)| (unit(mix64(salt ^ i as u64)) as f32, i)).collect()
    } else {
        let n = |i: usize| if st.noise.sort { noise.get(i).copied().unwrap_or(0.0) } else { 0.0 };
        px.iter().enumerate().map(|(i, &p)| (key(p, st.sort_by) + n(i), i)).collect()
    };
    order.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    if st.reverse && st.sort_by != RANDOM {
        order.reverse();
    }
    let mut out: Vec<Px> = order.iter().filter_map(|&(_, i)| px.get(i).copied()).collect();
    if st.fill == 1 && out.len() > 1 {
        let (Some(&a), Some(&b)) = (out.first(), out.last()) else { return out };
        let last = (out.len() - 1) as f32;
        for (j, o) in out.iter_mut().enumerate() {
            let t = j as f32 / last;
            *o = [0, 1, 2, 3].map(|c| a[c] + (b[c] - a[c]) * t);
        }
    }
    if st.mirror { mirrored(&out) } else { out }
}

/// A sorted run laid out up to the middle and back: even ranks from the start, odd ranks from
/// the end, so both ends hold the lowest keys and the middle the highest.
fn mirrored(sorted: &[Px]) -> Vec<Px> {
    let n = sorted.len();
    let mut out = sorted.to_vec();
    for (r, &px) in sorted.iter().enumerate() {
        let pos = if r % 2 == 0 { r / 2 } else { n - 1 - r / 2 };
        if let Some(o) = out.get_mut(pos) {
            *o = px;
        }
    }
    out
}

/// The new pixels of one line, in line order (`None` when nothing on it changed).
fn sort_line(line: &LineData, st: &Settings, line_id: i64) -> Option<Vec<Px>> {
    let mut out = line.px.clone();
    let mut changed = false;
    // Particle heads leave from the bright end of sorted spans: (span end, colour, salt).
    let mut emitters: Vec<(usize, Px, u64)> = Vec::new();
    for (s, e) in spans(line, st, line_id) {
        let n = e - s;
        if n < 2 || n < st.min_len {
            continue;
        }
        let (Some(src), Some(wts)) = (line.px.get(s..e), line.weight.get(s..e)) else { continue };
        let noise = line.noise.get(s..e).unwrap_or(&[]);
        let salt = mix64(st.seed ^ mix64((line_id as u64).wrapping_mul(0x632B_E59B_D9B4_E019) ^ s as u64));
        let sorted = sorted_span(src, noise, st, salt);
        if st.particles.amount > 0.0 && unit(mix64(salt ^ 0x7061_7274)) < st.particles.amount {
            let bright = src.iter().copied().max_by(|a, b| key(*a, 0).total_cmp(&key(*b, 0)));
            if let Some(c) = bright {
                emitters.push((if st.reverse { s } else { e - 1 }, c, salt));
            }
        }
        for (j, ((&orig, &new), &w)) in src.iter().zip(&sorted).zip(wts).enumerate() {
            let edge = (j.min(n - 1 - j) as f64 + 0.5) / st.feather.max(1e-9);
            let feather = if st.feather > 0.0 { edge.min(1.0) as f32 } else { 1.0 };
            let t = (st.amount * feather * w.clamp(0.0, 1.0)).clamp(0.0, 1.0);
            let Some(o) = out.get_mut(s + j) else { continue };
            // Fully sorted pixels move exactly (a lerp at 1 can round).
            if t >= 1.0 {
                *o = new;
            } else if t > 0.0 {
                *o = [0, 1, 2, 3].map(|c| orig[c] + (new[c] - orig[c]) * t);
            } else {
                continue;
            }
            changed = true;
        }
    }
    changed |= particles(&mut out, &emitters, st);
    changed.then_some(out)
}

/// Draws the particle streaks of `emitters` onto a line; whether anything was drawn.
fn particles(out: &mut [Px], emitters: &[(usize, Px, u64)], st: &Settings) -> bool {
    let p = st.particles;
    let n = out.len() as i64;
    let mut drew = false;
    for &(at, c, salt) in emitters {
        // Each particle starts at its own phase, so they don't leave in step.
        let phase = unit(mix64(salt ^ 0x6661_7365)) * p.distance;
        let head = (p.travelled + phase).rem_euclid(p.distance);
        if !head.is_finite() {
            continue;
        }
        let life = (1.0 - head / p.distance).clamp(0.0, 1.0) as f32;
        let step: i64 = if st.reverse { -1 } else { 1 };
        let len = p.length.min(MAX_LENGTH) as i64;
        for j in 0..len.max(1) {
            let pos = at as i64 + step * (head.round() as i64 - j);
            if pos < 0 || pos >= n {
                continue;
            }
            let tail = 1.0 - j as f32 / len.max(1) as f32;
            let k = p.opacity * life * tail * st.amount;
            let Some(o) = out.get_mut(pos as usize) else { continue };
            *o = if p.add {
                [o[0] + c[0] * k, o[1] + c[1] * k, o[2] + c[2] * k, (o[3] + c[3] * k).min(1.0)]
            } else {
                [0, 1, 2, 3].map(|i| o[i] + (c[i] - o[i]) * k)
            };
            drew = true;
        }
    }
    drew
}

/// The Constraint Layer's value per buffer pixel (`None` without a constraint layer).
fn constraint(ctx: &EffectCtx, b: &Buf) -> Option<Vec<f32>> {
    let other = ctx.layer_param("constraintLayer", false)?;
    let img = crate::util::fit_layer(ctx, b, &other, false);
    if img.data.len() != b.img.data.len() {
        return None;
    }
    let alpha = ctx.params.get("constraintChannel").map(Value::as_enum) == Some(1);
    let invert = ctx.params.get("invertConstraint").is_some_and(Value::as_bool);
    Some(
        img.data
            .iter()
            .map(|&p| {
                let v = if alpha { p[3] } else { luminance(p[0], p[1], p[2]) };
                let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
                if invert { 1.0 - v } else { v }
            })
            .collect(),
    )
}

/// 1 inside the layer's closed masks (outside inverted ones), 0 elsewhere, per buffer pixel.
fn mask_weight(ctx: &EffectCtx, b: &Buf) -> Vec<f32> {
    let (w, h) = (b.img.width as usize, b.img.height as usize);
    let s = b.scale.max(1e-9);
    let masks: Vec<&crate::MaskShape> =
        ctx.env.masks.iter().filter(|m| m.closed && m.points.len() > 2 && m.points.iter().flatten().all(|v| v.is_finite())).collect();
    if masks.is_empty() {
        return vec![0.0; w * h];
    }
    (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (((i % w) as f64 + 0.5 - b.offset[0]) / s, ((i / w) as f64 + 0.5 - b.offset[1]) / s);
            let inside = masks.iter().any(|m| crate::util::point_in_poly(&m.points, x, y) != m.inverted);
            if inside { 1.0 } else { 0.0 }
        })
        .collect()
}

fn pixel_sort(ctx: &EffectCtx, b: Buf) -> Buf {
    let (w, h) = (b.img.width as usize, b.img.height as usize);
    if w == 0 || h == 0 || b.img.data.len() != w * h {
        return b;
    }
    let st = settings(ctx, b.scale);
    if st.amount <= 0.0 {
        return b;
    }
    let mut st = st;
    let mut weight = constraint(ctx, &b).filter(|c| c.len() == w * h);
    if ctx.params.b("constrainToMasks") {
        let m = mask_weight(ctx, &b);
        weight = Some(match weight {
            Some(c) => c.iter().zip(&m).map(|(a, b)| a * b).collect(),
            None => m,
        });
    }
    let a = ctx.params.f("direction").to_radians();
    let a = if a.is_finite() { a } else { 0.0 };
    // 0° points up and angles turn clockwise, as for other direction controls.
    let dir = [a.sin(), -a.cos()];
    extras(ctx, b.scale, dir, &mut st);
    let lines = Lines::new(w, h, dir);
    let src = &b.img.data;
    let changes: Vec<(Vec<usize>, Vec<Px>)> = (lines.k_min..=lines.k_max)
        .into_par_iter()
        .filter_map(|k| {
            let idx = lines.indices(k);
            let px = idx.iter().filter_map(|&i| src.get(i).copied()).collect();
            let weight = match &weight {
                Some(c) => idx.iter().map(|&i| c.get(i).copied().unwrap_or(0.0)).collect(),
                None => vec![1.0; idx.len()],
            };
            let noise = if st.noise.amount > 0.0 { idx.iter().map(|&i| st.noise.at(i % w, i / w)).collect() } else { Vec::new() };
            let out = sort_line(&LineData { px, weight, noise }, &st, k)?;
            Some((idx, out))
        })
        .collect();
    let mut img: Image = b.img;
    for (idx, px) in changes {
        for (i, p) in idx.into_iter().zip(px) {
            if let Some(d) = img.data.get_mut(i) {
                *d = p;
            }
        }
    }
    Buf { img, offset: b.offset, scale: b.scale }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Params;

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

    /// Grey levels of a seeded pseudo-random `w`×`h` image (opaque).
    fn noise(w: u32, h: u32, seed: u64) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = unit(mix64(seed ^ u64::from(y * w + x))) as f32;
                img.set(x, y, [v, v * 0.5, 1.0 - v, 1.0]);
            }
        }
        img
    }

    fn lum_row(img: &Image, y: u32) -> Vec<f32> {
        (0..img.width).map(|x| key(at(img, x, y), 0)).collect()
    }

    fn sorted_multiset(mut v: Vec<[f32; 4]>) -> Vec<[u32; 4]> {
        let mut k: Vec<[u32; 4]> = v.drain(..).map(|p| p.map(f32::to_bits)).collect();
        k.sort_unstable();
        k
    }

    #[test]
    fn lines_cover_every_pixel_exactly_once_at_any_angle() {
        for (w, h) in [(1, 1), (7, 3), (3, 9), (16, 16), (31, 5)] {
            for deg in [0.0f64, 17.0, 45.0, 90.0, 133.0, 180.0, 200.0, 270.0, 300.0, 359.0] {
                let a = deg.to_radians();
                let l = Lines::new(w, h, [a.sin(), -a.cos()]);
                let mut seen = vec![0u32; w * h];
                for k in l.k_min..=l.k_max {
                    for i in l.indices(k) {
                        seen[i] += 1;
                    }
                }
                assert!(seen.iter().all(|&n| n == 1), "{w}x{h} at {deg}°: {seen:?}");
            }
        }
    }

    #[test]
    fn sort_all_orders_each_row_and_keeps_its_pixels() {
        let img = noise(40, 6, 3);
        let out = run(&[("lowerThreshold", num(0.0))], img.clone());
        for y in 0..6 {
            let row = lum_row(&out, y);
            assert!(row.windows(2).all(|p| p[0] <= p[1]), "row {y} ascends left to right: {row:?}");
            let before: Vec<_> = (0..40).map(|x| at(&img, x, y)).collect();
            let after: Vec<_> = (0..40).map(|x| at(&out, x, y)).collect();
            assert_eq!(sorted_multiset(before), sorted_multiset(after), "row {y} keeps its pixels");
        }
        // Reverse descends; 270° (left) ascends right to left, the same as Reverse at 90°.
        let rev = run(&[("lowerThreshold", num(0.0)), ("reverse", Value::Bool(true))], img.clone());
        assert!(lum_row(&rev, 2).windows(2).all(|p| p[0] >= p[1]));
        let left = run(&[("lowerThreshold", num(0.0)), ("direction", num(270.0))], img.clone());
        assert_eq!(left.data, rev.data);
        // 180° (down) sorts the columns.
        let down = run(&[("lowerThreshold", num(0.0)), ("direction", num(180.0))], img);
        for x in 0..40 {
            let col: Vec<f32> = (0..6).map(|y| key(at(&down, x, y), 0)).collect();
            assert!(col.windows(2).all(|p| p[0] <= p[1]), "column {x}");
        }
    }

    #[test]
    fn only_pixels_inside_the_thresholds_move() {
        // Dark (0.1) pixels split the row into bright runs; they must stay put.
        let mut img = Image::new(12, 1);
        let vals = [0.9, 0.5, 0.7, 0.1, 0.8, 0.6, 0.95, 0.1, 0.1, 0.4, 0.99, 0.6];
        for (x, v) in vals.iter().enumerate() {
            img.set(x as u32, 0, [*v, *v, *v, 1.0]);
        }
        let out = run(&[("lowerThreshold", num(30.0))], img);
        let got: Vec<f32> = (0..12).map(|x| at(&out, x, 0)[0]).collect();
        assert_eq!(got, [0.5, 0.7, 0.9, 0.1, 0.6, 0.8, 0.95, 0.1, 0.1, 0.4, 0.6, 0.99]);
        // Inverted, only the dark pixels form spans: 0.2 and 0.05 swap, the lone 0.1 stays.
        let mut img = Image::new(4, 1);
        for (x, v) in [0.2f32, 0.05, 0.9, 0.1].iter().enumerate() {
            img.set(x as u32, 0, [*v, *v, *v, 1.0]);
        }
        let out = run(&[("lowerThreshold", num(50.0)), ("invertSpans", Value::Bool(true))], img);
        let got: Vec<f32> = (0..4).map(|x| at(&out, x, 0)[0]).collect();
        assert_eq!(got, [0.05, 0.2, 0.9, 0.1]);
    }

    #[test]
    fn max_and_random_length_cut_spans() {
        let img = noise(64, 1, 9);
        let out = run(&[("lowerThreshold", num(0.0)), ("maxLength", num(8.0))], img.clone());
        let row = lum_row(&out, 0);
        for chunk in row.chunks(8) {
            assert!(chunk.windows(2).all(|p| p[0] <= p[1]), "each 8-pixel block is sorted on its own");
        }
        assert!(row.windows(2).any(|p| p[0] > p[1]), "but not the whole row");
        let a = run(&[("lowerThreshold", num(0.0)), ("maxLength", num(16.0)), ("randomLength", num(80.0))], img.clone());
        let b = run(&[("lowerThreshold", num(0.0)), ("maxLength", num(16.0)), ("randomLength", num(80.0))], img.clone());
        assert_eq!(a.data, b.data, "deterministic");
        let c = run(&[("lowerThreshold", num(0.0)), ("maxLength", num(16.0)), ("randomLength", num(80.0)), ("randomSeed", num(5.0))], img);
        assert_ne!(a.data, c.data, "the seed changes the cuts");
    }

    #[test]
    fn random_gradient_feather_and_blend() {
        let img = noise(32, 4, 21);
        // Random shuffles but keeps the pixels.
        let r = run(&[("lowerThreshold", num(0.0)), ("sortBy", Value::Enum(RANDOM))], img.clone());
        assert_ne!(r.data, img.data);
        assert_eq!(sorted_multiset(r.data.clone()), sorted_multiset(img.data.clone()));
        // Gradient: each row is a straight ramp between its darkest and brightest pixel.
        let g = run(&[("lowerThreshold", num(0.0)), ("fill", Value::Enum(1))], img.clone());
        let row: Vec<f32> = (0..32).map(|x| at(&g, x, 1)[0]).collect();
        let step = row[1] - row[0];
        assert!(row.windows(2).all(|p| ((p[1] - p[0]) - step).abs() < 1e-5), "{row:?}");
        // Feather: span ends stay closer to the original than the middle does.
        let f = run(&[("lowerThreshold", num(0.0)), ("feather", num(10.0))], img.clone());
        let s = run(&[("lowerThreshold", num(0.0))], img.clone());
        let d = |x: u32| (at(&f, x, 0)[0] - at(&img, x, 0)[0]).abs();
        let full = |x: u32| (at(&s, x, 0)[0] - at(&img, x, 0)[0]).abs();
        assert!(d(0) <= full(0) * 0.06 + 1e-6, "the first pixel barely moves");
        assert!((d(16) - full(16)).abs() < 1e-6, "the middle is fully sorted");
        // Blend With Original 100 % leaves the image alone; 50 % is halfway.
        assert_eq!(run(&[("lowerThreshold", num(0.0)), ("blend", num(100.0))], img.clone()).data, img.data);
        let half = run(&[("lowerThreshold", num(0.0)), ("blend", num(50.0))], img.clone());
        let x = 5;
        assert!((at(&half, x, 0)[0] - (at(&img, x, 0)[0] + at(&s, x, 0)[0]) * 0.5).abs() < 1e-5);
    }

    #[test]
    fn transparent_pixels_break_spans_and_alpha_moves_with_colour() {
        let mut img = Image::new(6, 1);
        let px = [[0.9, 0.9, 0.9, 1.0], [0.25, 0.25, 0.25, 0.5], [0.0; 4], [0.8, 0.8, 0.8, 1.0], [0.3, 0.3, 0.3, 1.0], [0.6, 0.6, 0.6, 1.0]];
        for (x, p) in px.iter().enumerate() {
            img.set(x as u32, 0, *p);
        }
        let out = run(&[("lowerThreshold", num(0.0))], img);
        let got: Vec<Px> = (0..6).map(|x| at(&out, x, 0)).collect();
        // Straight luminance 0.5 < 0.9; the half-transparent pixel keeps its alpha.
        assert_eq!(got[0], [0.25, 0.25, 0.25, 0.5]);
        assert_eq!(got[1], [0.9, 0.9, 0.9, 1.0]);
        assert_eq!(got[2], [0.0; 4]);
        assert_eq!(got[3..], [[0.3, 0.3, 0.3, 1.0], [0.6, 0.6, 0.6, 1.0], [0.8, 0.8, 0.8, 1.0]]);
    }

    /// Hostile values (NaN pixels, huge and negative lengths, NaN parameters, empty and
    /// one-pixel buffers) render without a panic and keep the buffer size.
    #[test]
    fn hostile_input_does_not_panic() {
        let mut img = noise(9, 5, 1);
        img.set(3, 2, [f32::NAN, f32::INFINITY, -1.0, 1.0]);
        for vals in [
            vec![("maxLength", num(1e300)), ("minLength", num(-5.0)), ("feather", num(f64::NAN))],
            vec![("direction", num(f64::NAN)), ("lowerThreshold", num(f64::INFINITY)), ("randomLength", num(-50.0))],
            vec![("direction", num(1e12)), ("randomSeed", num(f64::NAN)), ("sortBy", Value::Enum(99)), ("spanKey", Value::Enum(99))],
        ] {
            let out = run(&vals, img.clone());
            assert_eq!((out.width, out.height), (9, 5));
        }
        assert_eq!(run(&[], Image::new(1, 1)).data.len(), 1);
        assert!(run(&[], Image::new(0, 0)).data.is_empty());
    }

    #[test]
    fn stretch_smears_the_first_pixel_and_mirror_peaks_in_the_middle() {
        let img = noise(32, 3, 5);
        let st = run(&[("lowerThreshold", num(0.0)), ("fill", Value::Enum(2))], img.clone());
        for x in 0..32 {
            assert_eq!(at(&st, x, 1), at(&img, 0, 1), "every pixel is the row's first");
        }
        let rev = run(&[("lowerThreshold", num(0.0)), ("fill", Value::Enum(2)), ("reverse", Value::Bool(true))], img.clone());
        assert_eq!(at(&rev, 0, 1), at(&img, 31, 1), "Reverse smears the last one");
        let m = run(&[("lowerThreshold", num(0.0)), ("mirror", Value::Bool(true))], img.clone());
        let row = lum_row(&m, 1);
        let peak = row.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, _)| i).unwrap();
        assert!((14..=17).contains(&peak), "brightest in the middle: {peak}");
        assert!(row[..peak].windows(2).all(|p| p[0] <= p[1]) && row[peak..].windows(2).all(|p| p[0] >= p[1]), "{row:?}");
        let before: Vec<_> = (0..32).map(|x| at(&img, x, 1)).collect();
        let after: Vec<_> = (0..32).map(|x| at(&m, x, 1)).collect();
        assert_eq!(sorted_multiset(before), sorted_multiset(after), "mirroring keeps the pixels");
    }

    fn run_masked(vals: &[(&str, Value)], img: Image, masks: &[crate::MaskShape]) -> Image {
        let s = crate::find(ID).unwrap();
        let mut params = Params { values: s.params.iter().map(|p| (p.id.to_string(), p.default.clone())).collect() };
        for (k, v) in vals {
            params.values.insert(k.to_string(), v.clone());
        }
        let env = crate::EffectEnv { masks, ..Default::default() };
        let ctx = EffectCtx { params: &params, time: 0.0, layer_size: [img.width as f64, img.height as f64], seed: 1, adjustment: false, env };
        crate::apply(s, &ctx, Buf { img, offset: [0.0, 0.0], scale: 1.0 }).img
    }

    #[test]
    fn masks_constrain_the_sort() {
        let img = noise(40, 10, 8);
        let left = crate::MaskShape { name: "m".into(), points: vec![[0.0, 0.0], [20.0, 0.0], [20.0, 10.0], [0.0, 10.0]], closed: true, inverted: false };
        let vals = [("lowerThreshold", num(0.0)), ("constrainToMasks", Value::Bool(true))];
        let out = run_masked(&vals, img.clone(), std::slice::from_ref(&left));
        for y in 0..10 {
            let row = lum_row(&out, y);
            assert!(row[..20].windows(2).all(|p| p[0] <= p[1]), "inside sorted");
            for x in 20..40 {
                assert_eq!(at(&out, x, y), at(&img, x, y), "outside untouched");
            }
        }
        // Inverted: only the right half sorts; no masks at all: nothing sorts.
        let inv = crate::MaskShape { inverted: true, ..left };
        let out = run_masked(&vals, img.clone(), &[inv]);
        assert_eq!(at(&out, 3, 4), at(&img, 3, 4));
        assert!(lum_row(&out, 4)[20..].windows(2).all(|p| p[0] <= p[1]));
        assert_eq!(run_masked(&vals, img.clone(), &[]).data, img.data);
    }

    #[test]
    fn noise_roughens_spans_and_streaks() {
        let img = noise(48, 24, 2);
        let base = [("lowerThreshold", num(40.0))];
        let plain = run(&base, img.clone());
        let noisy = run(&[base[0].clone(), ("psNoise/amount", num(60.0))], img.clone());
        assert_ne!(plain.data, noisy.data);
        let ev = run(&[base[0].clone(), ("psNoise/amount", num(60.0)), ("psNoise/evolution", num(3.0))], img.clone());
        assert_ne!(noisy.data, ev.data, "evolution moves the noise");
        // Sort Key Only keeps the spans: each row still holds the same pixels.
        let sk = run(&[("lowerThreshold", num(0.0)), ("psNoise/amount", num(60.0)), ("psNoise/target", Value::Enum(2))], img.clone());
        for y in 0..24 {
            let b: Vec<_> = (0..48).map(|x| at(&img, x, y)).collect();
            let a: Vec<_> = (0..48).map(|x| at(&sk, x, y)).collect();
            assert_eq!(sorted_multiset(b), sorted_multiset(a));
        }
    }

    fn run_at(vals: &[(&str, Value)], img: Image, time: f64) -> Image {
        let s = crate::find(ID).unwrap();
        let mut params = Params { values: s.params.iter().map(|p| (p.id.to_string(), p.default.clone())).collect() };
        for (k, v) in vals {
            params.values.insert(k.to_string(), v.clone());
        }
        let ctx = EffectCtx { params: &params, time, layer_size: [img.width as f64, img.height as f64], seed: 1, adjustment: false, env: Default::default() };
        crate::apply(s, &ctx, Buf { img, offset: [0.0, 0.0], scale: 1.0 }).img
    }

    #[test]
    fn particles_leave_the_bright_end_and_move_over_time() {
        // Bright spans on the left of a dark field: particles travel right into the dark.
        let mut img = Image::new(80, 6);
        for y in 0..6 {
            for x in 0..80 {
                let v = if x < 20 { 0.5 + 0.02 * (x % 7) as f32 } else { 0.0 };
                img.set(x, y, [v, v, v, 1.0]);
            }
        }
        let vals = [("lowerThreshold", num(30.0)), ("psParticles/amount", num(100.0)), ("psParticles/speed", num(40.0)), ("psParticles/distance", num(50.0))];
        let dark = |o: &Image| (0..6).flat_map(|y| (25..80).map(move |x| (x, y))).map(|(x, y)| at(o, x, y)[0]).sum::<f32>();
        let a = run_at(&vals, img.clone(), 0.2);
        let b = run_at(&vals, img.clone(), 0.5);
        assert!(dark(&a) > 0.0 && dark(&b) > 0.0, "streaks reach the dark part");
        assert_ne!(a.data, b.data, "and move");
        assert_eq!(dark(&run_at(&vals[..1], img, 0.2)), 0.0, "none without particles");
    }

    /// The new options with hostile values.
    #[test]
    fn hostile_extras_do_not_panic() {
        let img = noise(13, 7, 4);
        for vals in [
            vec![("psNoise/amount", num(f64::NAN)), ("psNoise/scale", num(-1.0)), ("psNoise/stretch", num(f64::INFINITY)), ("psNoise/evolution", num(1e300))],
            vec![("psNoise/amount", num(100.0)), ("psNoise/scale", num(0.0)), ("psNoise/target", Value::Enum(77))],
            vec![
                ("psParticles/amount", num(100.0)),
                ("psParticles/length", num(1e300)),
                ("psParticles/speed", num(f64::NAN)),
                ("psParticles/distance", num(0.0)),
            ],
            vec![("psParticles/amount", num(1e9)), ("psParticles/speed", num(-1e300)), ("fill", Value::Enum(9)), ("mirror", Value::Bool(true))],
        ] {
            for t in [f64::NAN, 0.0, 1e300, -1e300] {
                let out = run_at(&vals, img.clone(), t);
                assert_eq!((out.width, out.height), (13, 7));
            }
        }
        let bad = crate::MaskShape { name: String::new(), points: vec![[f64::NAN, 0.0], [1.0, f64::INFINITY], [2.0, 2.0]], closed: true, inverted: false };
        let out = run_masked(&[("constrainToMasks", Value::Bool(true))], img, &[bad]);
        assert_eq!(out.data.len(), 13 * 7);
    }
}
