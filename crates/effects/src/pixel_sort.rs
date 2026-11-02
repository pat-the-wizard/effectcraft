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
const FILLS: &[&str] = &["Sorted Pixels", "Gradient"];
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
            p("feather", "Edge Feather", num(0.0), slider(0.0, 1000.0, 0.0, 100.0, 1)),
            p("constraintLayer", "Constraint Layer", Value::Layer(None), ParamUi::Layer),
            p("constraintChannel", "Constraint Channel", Value::Enum(0), popup(CONSTRAINT_CHANNELS)),
            p("invertConstraint", "Invert Constraint", Value::Bool(false), ParamUi::Checkbox),
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
    gradient: bool,
    feather: f64,
    amount: f32,
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
        gradient: e("fill") == 1,
        feather: (f("feather") * scale).max(0.0),
        amount: (1.0 - f("blend") / 100.0).clamp(0.0, 1.0) as f32,
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
struct LineData<'a> {
    idx: &'a [usize],
    px: Vec<Px>,
    /// The Constraint Layer's value per pixel (1 without one).
    weight: Vec<f32>,
}

/// Where spans start and end on a line (`[start, end)` in line positions).
fn spans(line: &LineData, st: &Settings, line_id: i64) -> Vec<(usize, usize)> {
    let selected = |i: usize| -> bool {
        let (Some(&px), Some(&w)) = (line.px.get(i), line.weight.get(i)) else { return false };
        if w < 0.5 || (px[3] <= 1e-6 && st.span_key != 7) {
            return false;
        }
        let v = key(px, st.span_key);
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
fn sorted_span(px: &[Px], st: &Settings, salt: u64) -> Vec<Px> {
    let mut order: Vec<(f32, usize)> = if st.sort_by == RANDOM {
        px.iter().enumerate().map(|(i, _)| (unit(mix64(salt ^ i as u64)) as f32, i)).collect()
    } else {
        px.iter().enumerate().map(|(i, &p)| (key(p, st.sort_by), i)).collect()
    };
    order.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    if st.reverse && st.sort_by != RANDOM {
        order.reverse();
    }
    let mut out: Vec<Px> = order.iter().filter_map(|&(_, i)| px.get(i).copied()).collect();
    if st.gradient && out.len() > 1 {
        let (Some(&a), Some(&b)) = (out.first(), out.last()) else { return out };
        let last = (out.len() - 1) as f32;
        for (j, o) in out.iter_mut().enumerate() {
            let t = j as f32 / last;
            *o = [0, 1, 2, 3].map(|c| a[c] + (b[c] - a[c]) * t);
        }
    }
    out
}

/// The new pixels of one line: `(buffer index, pixel)` for every pixel a span changed.
fn sort_line(line: &LineData, st: &Settings, line_id: i64) -> Vec<(usize, Px)> {
    let mut out = Vec::new();
    for (s, e) in spans(line, st, line_id) {
        let n = e - s;
        if n < 2 || n < st.min_len {
            continue;
        }
        let (Some(src), Some(idx), Some(wts)) = (line.px.get(s..e), line.idx.get(s..e), line.weight.get(s..e)) else { continue };
        let salt = mix64(st.seed ^ mix64((line_id as u64).wrapping_mul(0x632B_E59B_D9B4_E019) ^ s as u64));
        let sorted = sorted_span(src, st, salt);
        for (j, ((&orig, &bi), (&new, &w))) in src.iter().zip(idx).zip(sorted.iter().zip(wts)).enumerate() {
            let edge = (j.min(n - 1 - j) as f64 + 0.5) / st.feather.max(1e-9);
            let feather = if st.feather > 0.0 { edge.min(1.0) as f32 } else { 1.0 };
            let t = (st.amount * feather * w.clamp(0.0, 1.0)).clamp(0.0, 1.0);
            // Fully sorted pixels move exactly (a lerp at 1 can round).
            if t >= 1.0 {
                out.push((bi, new));
            } else if t > 0.0 {
                out.push((bi, [0, 1, 2, 3].map(|c| orig[c] + (new[c] - orig[c]) * t)));
            }
        }
    }
    out
}

/// The Constraint Layer's value per buffer pixel (`None` without a constraint layer).
fn constraint(ctx: &EffectCtx, b: &Buf) -> Option<Vec<f32>> {
    let other = ctx.layer_param("constraintLayer", false)?;
    let img = crate::util::fit_layer(ctx, b, &other, false);
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

fn pixel_sort(ctx: &EffectCtx, b: Buf) -> Buf {
    let (w, h) = (b.img.width as usize, b.img.height as usize);
    if w == 0 || h == 0 || b.img.data.len() != w * h {
        return b;
    }
    let st = settings(ctx, b.scale);
    if st.amount <= 0.0 {
        return b;
    }
    let weight = constraint(ctx, &b).filter(|c| c.len() == w * h);
    let a = ctx.params.f("direction").to_radians();
    let a = if a.is_finite() { a } else { 0.0 };
    // 0° points up and angles turn clockwise, as for other direction controls.
    let lines = Lines::new(w, h, [a.sin(), -a.cos()]);
    let src = &b.img.data;
    let changes: Vec<Vec<(usize, Px)>> = (lines.k_min..=lines.k_max)
        .into_par_iter()
        .map(|k| {
            let idx = lines.indices(k);
            let px = idx.iter().filter_map(|&i| src.get(i).copied()).collect();
            let weight = match &weight {
                Some(c) => idx.iter().map(|&i| c.get(i).copied().unwrap_or(0.0)).collect(),
                None => vec![1.0; idx.len()],
            };
            sort_line(&LineData { idx: &idx, px, weight }, &st, k)
        })
        .collect();
    let mut img: Image = b.img;
    for (i, p) in changes.into_iter().flatten() {
        if let Some(d) = img.data.get_mut(i) {
            *d = p;
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
}
