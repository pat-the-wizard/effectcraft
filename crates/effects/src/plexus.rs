//! Plexus: an original EffectCraft effect (Generate), not an After Effects one. It builds a cloud
//! of points in 3D, moves it with effectors and draws points, the lines between nearby points and
//! the facets between nearby triples: the "plexus" network look of motion design.
//!
//! Written from the published descriptions of plexus plug-ins (geometry objects that make
//! points, effectors that move and colour them, and render objects for points, lines connecting
//! points within a maximum distance to a number of nearest points, opacity and thickness over
//! distance, and facets) and from general 3D graphics; the code, parameters and maths are our
//! own.
//!
//! * **Geometry** makes the points in layer pixels around Center: random in a box or sphere,
//!   on a sphere's surface (Fibonacci spiral), a grid, the pixels of a layer (weighted by its
//!   luminance or alpha above a threshold, coloured by them) or along the layer's masks.
//! * **Effectors**, in order: Noise (smooth 3D value noise that evolves over time), Transform
//!   (rotation, spin per second and scale about Center) and Color (single, by depth, random,
//!   or the source pixels' colours), then Depth Fade.
//! * **Camera**: the comp's camera when one is active and Use Comp Camera is on, else a camera
//!   looking straight at the layer with the given angle of view (the z = 0 plane keeps its size).
//! * **Render objects**: each point connects to up to Max Points to Search of its nearest points
//!   within [Min, Max] Distance (3D, layer pixels; pairs are found with a spatial hash).
//!   Opacity and thickness fall off over distance. Facets fill the triangles whose three sides
//!   are all within Facet Max Distance. Everything is drawn far to near, antialiased, in parallel
//!   bands, Normal or Add, over the layer or on its own.

use effectcraft_color::luminance;
use effectcraft_keyframe::Value;
use effectcraft_project::ParamUi;
use effectcraft_raster::{Image, Px};
use rayon::prelude::*;

use crate::card3d::{Proj, comp_proj};
use crate::util::unpremul;
use crate::{Buf, EffectCtx, EffectSpec, col, num, p, popup, slider};

pub const ID: &str = "ec.generate.plexus";

/// Twirl-down groups (id, display name).
pub const GROUPS: &[(&str, &str)] = &[
    ("plxGeometry", "Geometry"),
    ("plxNoise", "Noise Effector"),
    ("plxTransform", "Transform Effector"),
    ("plxColor", "Color Effector"),
    ("plxCamera", "Camera"),
    ("plxPoints", "Points"),
    ("plxLines", "Lines"),
    ("plxFacets", "Facets"),
];

const SOURCES: &[&str] = &["Random in Box", "Random in Sphere", "Sphere Surface", "Grid", "Layer Pixels", "Along Masks"];
const FALLOFFS: &[&str] = &["None", "Linear", "Smooth", "Strong"];
const COLOR_MODES: &[&str] = &["Single Color", "Depth Gradient", "Random Between Colors", "Source Pixels"];

/// Most points the geometry makes.
pub const MAX_POINTS: usize = 20_000;
/// Most lines and facets drawn (the nearest pairs and triples win).
const MAX_LINES: usize = 200_000;
const MAX_FACETS: usize = 100_000;
/// Nearest points each point connects to when facets are found.
const FACET_NEIGHBOURS: usize = 6;

fn pct() -> ParamUi {
    slider(0.0, 100.0, 0.0, 100.0, 1)
}

pub fn specs() -> Vec<EffectSpec> {
    let params = vec![
        // Geometry
        p("plxGeometry/source", "Source", Value::Enum(0), popup(SOURCES)),
        p("plxGeometry/count", "Number of Points", num(150.0), slider(1.0, MAX_POINTS as f64, 1.0, 1000.0, 0)),
        p("plxGeometry/center", "Center", Value::Vec2([0.5, 0.5]), ParamUi::Point),
        p("plxGeometry/centerZ", "Center Z", num(0.0), slider(-100_000.0, 100_000.0, -1000.0, 1000.0, 1)),
        p("plxGeometry/width", "Width", num(100.0), slider(0.0, 1000.0, 0.0, 200.0, 1)),
        p("plxGeometry/height", "Height", num(100.0), slider(0.0, 1000.0, 0.0, 200.0, 1)),
        p("plxGeometry/depth", "Depth", num(400.0), slider(0.0, 100_000.0, 0.0, 2000.0, 1)),
        p("plxGeometry/columns", "Grid Columns", num(12.0), slider(1.0, 500.0, 1.0, 50.0, 0)),
        p("plxGeometry/rows", "Grid Rows", num(8.0), slider(1.0, 500.0, 1.0, 50.0, 0)),
        p("plxGeometry/layersZ", "Grid Layers", num(1.0), slider(1.0, 100.0, 1.0, 10.0, 0)),
        p("plxGeometry/layer", "Source Layer", Value::Layer(None), ParamUi::Layer),
        p("plxGeometry/channel", "Use", Value::Enum(0), popup(&["Luminance", "Alpha"])),
        p("plxGeometry/threshold", "Threshold", num(50.0), pct()),
        p("plxGeometry/seed", "Random Seed", num(0.0), slider(0.0, 10_000.0, 0.0, 1000.0, 0)),
        // Noise effector
        p("plxNoise/amount", "Amplitude", num(40.0), slider(0.0, 10_000.0, 0.0, 300.0, 1)),
        p("plxNoise/scale", "Noise Scale", num(300.0), slider(1.0, 10_000.0, 10.0, 1000.0, 1)),
        p("plxNoise/speed", "Evolution Speed", num(0.3), slider(0.0, 100.0, 0.0, 5.0, 2)),
        p("plxNoise/amountZ", "Z Amount", num(100.0), slider(0.0, 400.0, 0.0, 200.0, 1)),
        // Transform effector
        p("plxTransform/rotationX", "X Rotation", num(0.0), ParamUi::Angle),
        p("plxTransform/rotationY", "Y Rotation", num(0.0), ParamUi::Angle),
        p("plxTransform/rotationZ", "Z Rotation", num(0.0), ParamUi::Angle),
        p("plxTransform/spinX", "X Spin per Second", num(0.0), slider(-3600.0, 3600.0, -90.0, 90.0, 1)),
        p("plxTransform/spinY", "Y Spin per Second", num(10.0), slider(-3600.0, 3600.0, -90.0, 90.0, 1)),
        p("plxTransform/spinZ", "Z Spin per Second", num(0.0), slider(-3600.0, 3600.0, -90.0, 90.0, 1)),
        p("plxTransform/scale", "Scale", num(100.0), slider(0.0, 1000.0, 0.0, 200.0, 1)),
        // Color effector
        p("plxColor/mode", "Color Mode", Value::Enum(1), popup(COLOR_MODES)),
        p("plxColor/colorA", "Color (Near)", col(0.55, 0.85, 1.0), ParamUi::Color),
        p("plxColor/colorB", "Color (Far)", col(0.25, 0.3, 0.9), ParamUi::Color),
        p("plxColor/depthFade", "Depth Fade", num(60.0), pct()),
        // Camera
        p("plxCamera/useComp", "Use Comp Camera", Value::Bool(true), ParamUi::Checkbox),
        p("plxCamera/fov", "Angle of View", num(39.6), slider(1.0, 170.0, 10.0, 120.0, 1)),
        // Points
        p("plxPoints/on", "Render Points", Value::Bool(true), ParamUi::Checkbox),
        p("plxPoints/size", "Point Size", num(4.0), slider(0.0, 500.0, 0.0, 30.0, 1)),
        p("plxPoints/opacity", "Point Opacity", num(100.0), pct()),
        p("plxPoints/softness", "Softness", num(30.0), pct()),
        p("plxPoints/scaleByDepth", "Scale by Depth", Value::Bool(true), ParamUi::Checkbox),
        // Lines
        p("plxLines/on", "Render Lines", Value::Bool(true), ParamUi::Checkbox),
        p("plxLines/maxDistance", "Max Distance", num(140.0), slider(0.0, 100_000.0, 0.0, 600.0, 1)),
        p("plxLines/minDistance", "Min Distance", num(0.0), slider(0.0, 100_000.0, 0.0, 600.0, 1)),
        p("plxLines/maxPoints", "Max Points to Search", num(10.0), slider(1.0, 200.0, 1.0, 50.0, 0)),
        p("plxLines/thickness", "Line Thickness", num(1.2), slider(0.0, 100.0, 0.0, 10.0, 2)),
        p("plxLines/opacity", "Line Opacity", num(80.0), pct()),
        p("plxLines/opacityFalloff", "Opacity over Distance", Value::Enum(2), popup(FALLOFFS)),
        p("plxLines/thicknessFalloff", "Thickness over Distance", Value::Enum(0), popup(FALLOFFS)),
        p("plxLines/useVertexColors", "Use Vertex Colors", Value::Bool(true), ParamUi::Checkbox),
        p("plxLines/color", "Line Color", col(1.0, 1.0, 1.0), ParamUi::Color),
        // Facets
        p("plxFacets/on", "Render Facets", Value::Bool(false), ParamUi::Checkbox),
        p("plxFacets/maxDistance", "Facet Max Distance", num(110.0), slider(0.0, 100_000.0, 0.0, 600.0, 1)),
        p("plxFacets/opacity", "Facet Opacity", num(20.0), pct()),
        p("plxFacets/opacityFalloff", "Opacity over Distance", Value::Bool(true), ParamUi::Checkbox),
        p("plxFacets/useVertexColors", "Use Vertex Colors", Value::Bool(true), ParamUi::Checkbox),
        p("plxFacets/color", "Facet Color", col(0.4, 0.6, 1.0), ParamUi::Color),
        // Output
        p("blendMode", "Blend Mode", Value::Enum(0), popup(&["Normal", "Add"])),
        p("composite", "Composite on Original", Value::Bool(true), ParamUi::Checkbox),
    ];
    vec![EffectSpec { id: ID, name: "Plexus", category: "Generate", params, render: plexus, gpu: false, float: true }]
}

// ------------------------------------------------------------------ helpers

fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Seeded random stream (0..1).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = mix64(self.0);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A finite parameter value, else `d`.
fn fv(ctx: &EffectCtx, id: &str, d: f64) -> f64 {
    let v = ctx.params.get(id).map(Value::as_f64).unwrap_or(d);
    if v.is_finite() { v } else { d }
}

fn finite_color(c: [f32; 4]) -> [f32; 3] {
    [c[0], c[1], c[2]].map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 })
}

/// Smooth value noise in −1..1 at a 3D point (trilinear over a hashed lattice, smoothstepped).
fn noise3(p: [f64; 3], seed: u64) -> f64 {
    let i = p.map(f64::floor);
    let f = [p[0] - i[0], p[1] - i[1], p[2] - i[2]].map(|t| t * t * (3.0 - 2.0 * t));
    let lat = |dx: f64, dy: f64, dz: f64| {
        let h = mix64(seed ^ mix64((i[0] + dx) as i64 as u64 ^ mix64((i[1] + dy) as i64 as u64 ^ mix64((i[2] + dz) as i64 as u64))));
        (h >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    };
    let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
    let x00 = lerp(lat(0.0, 0.0, 0.0), lat(1.0, 0.0, 0.0), f[0]);
    let x10 = lerp(lat(0.0, 1.0, 0.0), lat(1.0, 1.0, 0.0), f[0]);
    let x01 = lerp(lat(0.0, 0.0, 1.0), lat(1.0, 0.0, 1.0), f[0]);
    let x11 = lerp(lat(0.0, 1.0, 1.0), lat(1.0, 1.0, 1.0), f[0]);
    lerp(lerp(x00, x10, f[1]), lerp(x01, x11, f[1]), f[2])
}

/// Falloff of a value over normalised distance `t` (0 near, 1 at Max Distance).
fn falloff(kind: u32, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    match kind {
        1 => 1.0 - t,
        2 => {
            let u = 1.0 - t;
            u * u * (3.0 - 2.0 * u)
        }
        3 => (1.0 - t).powi(3),
        _ => 1.0,
    }
}

fn rot3(a: [f64; 3]) -> [[f64; 3]; 3] {
    let (sx, cx) = a[0].sin_cos();
    let (sy, cy) = a[1].sin_cos();
    let (sz, cz) = a[2].sin_cos();
    // Z · Y · X (X applied first).
    [[cy * cz, sx * sy * cz - cx * sz, cx * sy * cz + sx * sz], [cy * sz, sx * sy * sz + cx * cz, cx * sy * sz - sx * cz], [-sy, sx * cy, cx * cy]]
}

// ------------------------------------------------------------------ geometry

/// A point: position (layer pixels, z away from the viewer) and an optional source colour.
#[derive(Clone, Copy, Debug)]
struct Vertex {
    pos: [f64; 3],
    color: Option<[f32; 3]>,
    rand: f64,
}

fn geometry(ctx: &EffectCtx, b: &Buf) -> Vec<Vertex> {
    let g = |id: &str, d: f64| fv(ctx, &format!("plxGeometry/{id}"), d);
    let count = g("count", 150.0).round().clamp(0.0, MAX_POINTS as f64) as usize;
    let [lw, lh] = ctx.layer_size;
    let c = ctx.params.v2("plxGeometry/center");
    let c = if c.iter().all(|v| v.is_finite()) { c } else { [lw * 0.5, lh * 0.5] };
    let cz = g("centerZ", 0.0);
    let (sw, sh, sd) = (lw * g("width", 100.0).max(0.0) / 100.0, lh * g("height", 100.0).max(0.0) / 100.0, g("depth", 400.0).max(0.0));
    let seed = (g("seed", 0.0).clamp(0.0, 1e9) as u64) ^ (u64::from(ctx.seed) << 32);
    let mut rng = Rng(mix64(seed));
    let mut pts = Vec::with_capacity(count);
    let at = |x: f64, y: f64, z: f64| [c[0] + x, c[1] + y, cz + z];
    match ctx.params.e("plxGeometry/source") {
        1 => {
            // Uniform in the ellipsoid (rejection from the cube).
            let mut tries = 0;
            while pts.len() < count && tries < count * 8 + 64 {
                tries += 1;
                let q = [rng.next() * 2.0 - 1.0, rng.next() * 2.0 - 1.0, rng.next() * 2.0 - 1.0];
                if q[0] * q[0] + q[1] * q[1] + q[2] * q[2] <= 1.0 {
                    let r = rng.next();
                    pts.push(Vertex { pos: at(q[0] * sw * 0.5, q[1] * sh * 0.5, q[2] * sd * 0.5), color: None, rand: r });
                }
            }
        }
        2 => {
            let golden = std::f64::consts::PI * (3.0 - 5f64.sqrt());
            for i in 0..count {
                let y = 1.0 - 2.0 * (i as f64 + 0.5) / count as f64;
                let r = (1.0 - y * y).max(0.0).sqrt();
                let th = golden * i as f64;
                pts.push(Vertex { pos: at(r * th.cos() * sw * 0.5, y * sh * 0.5, r * th.sin() * sd * 0.5), color: None, rand: rng.next() });
            }
        }
        3 => {
            let n = |id: &str, d: f64| g(id, d).round().clamp(1.0, 500.0) as usize;
            let (cols, rows, layers) = (n("columns", 12.0), n("rows", 8.0), n("layersZ", 1.0));
            let step = |n: usize, size: f64, i: usize| if n > 1 { (i as f64 / (n - 1) as f64 - 0.5) * size } else { 0.0 };
            'grid: for k in 0..layers {
                for j in 0..rows {
                    for i in 0..cols {
                        if pts.len() >= MAX_POINTS {
                            break 'grid;
                        }
                        pts.push(Vertex { pos: at(step(cols, sw, i), step(rows, sh, j), step(layers, sd, k)), color: None, rand: rng.next() });
                    }
                }
            }
        }
        4 => layer_points(ctx, b, count, sd, cz, &mut rng, &mut pts),
        5 => mask_points(ctx, count, sd, cz, &mut rng, &mut pts),
        _ => {
            for _ in 0..count {
                let q = [rng.next() - 0.5, rng.next() - 0.5, rng.next() - 0.5];
                pts.push(Vertex { pos: at(q[0] * sw, q[1] * sh, q[2] * sd), color: None, rand: rng.next() });
            }
        }
    }
    pts
}

/// Points on the pixels of the Source Layer (or the layer itself) whose luminance / alpha is
/// above the threshold, more where it is stronger, carrying the pixel's colour.
fn layer_points(ctx: &EffectCtx, b: &Buf, count: usize, depth: f64, cz: f64, rng: &mut Rng, pts: &mut Vec<Vertex>) {
    let other = ctx.layer_param("plxGeometry/layer", true).map(|o| crate::util::fit_layer(ctx, b, &o, false));
    let img: &Image = other.as_ref().unwrap_or(&b.img);
    let alpha = ctx.params.e("plxGeometry/channel") == 1;
    let thr = (fv(ctx, "plxGeometry/threshold", 50.0) / 100.0) as f32;
    let (w, h) = (img.width as usize, img.height as usize);
    if w == 0 || h == 0 {
        return;
    }
    // Cumulative weights over the pixels (strided on huge images).
    let step = ((w * h) / 4_000_000).max(1);
    let mut cum: Vec<(f64, usize)> = Vec::new();
    let mut total = 0.0;
    for i in (0..w * h).step_by(step) {
        let Some(&px) = img.data.get(i) else { continue };
        let (c, a) = unpremul(px);
        let v = if alpha { a } else { luminance(c[0], c[1], c[2]) * a };
        if v.is_finite() && v > thr {
            total += f64::from(v);
            cum.push((total, i));
        }
    }
    if cum.is_empty() || total <= 0.0 {
        return;
    }
    let s = b.scale.max(1e-9);
    for _ in 0..count {
        let r = rng.next() * total;
        let k = cum.partition_point(|e| e.0 < r).min(cum.len() - 1);
        let Some(&(_, i)) = cum.get(k) else { continue };
        let (bx, by) = ((i % w) as f64 + rng.next(), (i / w) as f64 + rng.next());
        let (c, _) = img.data.get(i).map(|&p| unpremul(p)).unwrap_or(([1.0; 3], 1.0));
        let pos = [(bx - b.offset[0]) / s, (by - b.offset[1]) / s, cz + (rng.next() - 0.5) * depth];
        pts.push(Vertex { pos, color: Some(c.map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 })), rand: rng.next() });
    }
}

/// `count` points spread evenly along the layer's masks (by length).
fn mask_points(ctx: &EffectCtx, count: usize, depth: f64, cz: f64, rng: &mut Rng, pts: &mut Vec<Vertex>) {
    let mut segs: Vec<([f64; 2], [f64; 2], f64)> = Vec::new();
    for m in ctx.env.masks {
        let n = m.points.len();
        let closed = usize::from(m.closed && n > 2);
        for i in 0..(n.saturating_sub(1) + closed) {
            let (Some(&a), Some(&b)) = (m.points.get(i), m.points.get((i + 1) % n)) else { continue };
            let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            if len.is_finite() && len > 0.0 {
                segs.push((a, b, len));
            }
        }
    }
    let total: f64 = segs.iter().map(|s| s.2).sum();
    if segs.is_empty() || total <= 0.0 || count == 0 {
        return;
    }
    let step = total / count as f64;
    let (mut si, mut into) = (0usize, 0.0);
    for k in 0..count {
        let mut want = k as f64 * step - into;
        while let Some(s) = segs.get(si) {
            if want <= s.2 {
                break;
            }
            want -= s.2;
            into += s.2;
            si += 1;
        }
        let Some(&(a, b, len)) = segs.get(si).or(segs.last()) else { break };
        let t = (want / len).clamp(0.0, 1.0);
        pts.push(Vertex { pos: [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, cz + (rng.next() - 0.5) * depth], color: None, rand: rng.next() });
    }
}

// ------------------------------------------------------------------ effectors and camera

/// A point as the camera sees it: buffer pixels, depth (larger = further) and pixels per layer
/// pixel there.
#[derive(Clone, Copy, Debug)]
struct Seen {
    xy: [f64; 2],
    depth: f64,
    scale: f64,
    world: [f64; 3],
    color: [f32; 3],
    alpha: f32,
}

fn effectors(ctx: &EffectCtx, pts: &mut [Vertex]) {
    let c = ctx.params.v2("plxGeometry/center");
    let c = if c.iter().all(|v| v.is_finite()) { c } else { [ctx.layer_size[0] * 0.5, ctx.layer_size[1] * 0.5] };
    let cz = fv(ctx, "plxGeometry/centerZ", 0.0);
    let t = if ctx.time.is_finite() { ctx.time } else { 0.0 };
    // Noise.
    let amp = fv(ctx, "plxNoise/amount", 40.0).max(0.0);
    if amp > 0.0 {
        let scale = fv(ctx, "plxNoise/scale", 300.0).max(1.0);
        let ev = t * fv(ctx, "plxNoise/speed", 0.3);
        let zf = fv(ctx, "plxNoise/amountZ", 100.0).max(0.0) / 100.0;
        let seed = mix64(u64::from(ctx.seed) ^ 0x006E_015E);
        pts.par_iter_mut().for_each(|v| {
            let q = [v.pos[0] / scale, v.pos[1] / scale, v.pos[2] / scale + ev];
            let d = [0u64, 1, 2].map(|k| noise3([q[0] + k as f64 * 31.7, q[1] - k as f64 * 17.3, q[2] + ev * 0.37], seed ^ k));
            v.pos = [v.pos[0] + d[0] * amp, v.pos[1] + d[1] * amp, v.pos[2] + d[2] * amp * zf];
        });
    }
    // Transform about Center.
    let deg = |id: &str| fv(ctx, &format!("plxTransform/rotation{id}"), 0.0) + fv(ctx, &format!("plxTransform/spin{id}"), 0.0) * t;
    let r = rot3([deg("X").to_radians(), deg("Y").to_radians(), deg("Z").to_radians()]);
    let k = fv(ctx, "plxTransform/scale", 100.0) / 100.0;
    if r != rot3([0.0; 3]) || k != 1.0 {
        for v in pts.iter_mut() {
            let d = [v.pos[0] - c[0], v.pos[1] - c[1], v.pos[2] - cz];
            let q = r.map(|row| (row[0] * d[0] + row[1] * d[1] + row[2] * d[2]) * k);
            v.pos = [c[0] + q[0], c[1] + q[1], cz + q[2]];
        }
    }
}

enum Camera {
    Comp(Proj),
    Own { c: [f64; 2], d: f64 },
}

impl Camera {
    fn new(ctx: &EffectCtx, b: &Buf) -> Camera {
        let [lw, lh] = ctx.layer_size;
        let c = [b.offset[0] + lw * 0.5 * b.scale, b.offset[1] + lh * 0.5 * b.scale];
        if ctx.params.b("plxCamera/useComp")
            && let Some(cam) = ctx.env.host.and_then(|h| h.comp_scene()).filter(|sc| sc.camera_layer).and_then(|sc| sc.camera)
        {
            return Camera::Comp(comp_proj(&cam, c, b));
        }
        let fov = fv(ctx, "plxCamera/fov", 39.6).clamp(1.0, 170.0).to_radians();
        let d = (0.5 * lw.max(1.0) * b.scale / (fov * 0.5).tan()).max(1e-6);
        Camera::Own { c, d }
    }

    /// Where buffer-world point `p` (buffer pixels, z in buffer pixels) lands.
    fn see(&self, p: [f64; 3]) -> Option<([f64; 2], f64, f64)> {
        match self {
            Camera::Comp(proj) => proj.view(p, [0.0; 3]).map(|s| (s.at, s.depth, s.scale)),
            Camera::Own { c, d } => {
                let w = p[2] + d;
                if !(w > 0.02 * d && w.is_finite()) {
                    return None;
                }
                let k = d / w;
                let at = [c[0] + (p[0] - c[0]) * k, c[1] + (p[1] - c[1]) * k];
                at.iter().all(|v| v.is_finite()).then_some((at, w, k))
            }
        }
    }
}

fn project(ctx: &EffectCtx, b: &Buf, pts: &[Vertex]) -> Vec<Option<Seen>> {
    let cam = Camera::new(ctx, b);
    let s = b.scale;
    let mut seen: Vec<Option<Seen>> = pts
        .iter()
        .map(|v| {
            let (xy, depth, scale) = cam.see([v.pos[0] * s + b.offset[0], v.pos[1] * s + b.offset[1], v.pos[2] * s])?;
            Some(Seen { xy, depth, scale, world: v.pos, color: [1.0; 3], alpha: 1.0 })
        })
        .collect();
    // Colour and depth fade, by depth across the visible points.
    let (lo, hi) = seen.iter().flatten().fold((f64::MAX, f64::MIN), |(lo, hi), s| (lo.min(s.depth), hi.max(s.depth)));
    let span = (hi - lo).max(1e-9);
    let a = finite_color(ctx.params.color("plxColor/colorA"));
    let bcol = finite_color(ctx.params.color("plxColor/colorB"));
    let mode = ctx.params.e("plxColor/mode");
    let fade = (fv(ctx, "plxColor/depthFade", 60.0) / 100.0).clamp(0.0, 1.0) as f32;
    let lerp = |t: f32| [0, 1, 2].map(|i| a[i] + (bcol[i] - a[i]) * t);
    for (s, v) in seen.iter_mut().zip(pts) {
        let Some(s) = s else { continue };
        let t = ((s.depth - lo) / span) as f32;
        s.color = match mode {
            1 => lerp(t),
            2 => lerp(v.rand as f32),
            3 => v.color.unwrap_or(a),
            _ => a,
        };
        s.alpha = 1.0 - fade * t;
    }
    seen
}

// ------------------------------------------------------------------ connections

/// Pairs (i < j) of points within [min, max] distance, each point keeping its `k` nearest.
///
/// The points go into a hash grid whose cells are about twice the typical spacing (never more
/// than `max`); each point searches rings of cells outwards and stops once it has `k` points
/// nearer than the next ring can hold, or the ring passes `max` or the cloud's extent.
fn connections(pts: &[Option<Seen>], min: f64, max: f64, k: usize) -> Vec<(usize, usize, f64)> {
    if max.is_nan() || max <= 0.0 || k == 0 {
        return Vec::new();
    }
    let live: Vec<(usize, [f64; 3])> =
        pts.iter().enumerate().filter_map(|(i, s)| s.as_ref().filter(|s| s.world.iter().all(|v| v.is_finite())).map(|s| (i, s.world))).collect();
    if live.len() < 2 {
        return Vec::new();
    }
    let (lo, hi) = live.iter().fold(([f64::MAX; 3], [f64::MIN; 3]), |(lo, hi), (_, p)| {
        ([lo[0].min(p[0]), lo[1].min(p[1]), lo[2].min(p[2])], [hi[0].max(p[0]), hi[1].max(p[1]), hi[2].max(p[2])])
    });
    let extent = (0..3).map(|a| hi[a] - lo[a]).fold(0.0, f64::max).max(1e-9);
    let spacing = extent / (live.len() as f64).cbrt();
    let cell = max.min(spacing * 2.0).max(extent * 1e-6).max(1e-9);
    let key = |p: [f64; 3]| [0, 1, 2].map(|a| ((p[a] - lo[a]) / cell).floor().clamp(0.0, 1e9) as i64);
    let span = key(hi).into_iter().max().unwrap_or(0);
    let mut grid: std::collections::HashMap<[i64; 3], Vec<usize>> = std::collections::HashMap::new();
    for &(i, p) in &live {
        grid.entry(key(p)).or_default().push(i);
    }
    // Rings to search: enough to reach `max`, never past the cloud.
    let rings = ((max / cell).ceil().min(span as f64 + 1.0)).max(1.0) as i64;
    let near: Vec<Vec<(usize, usize, f64)>> = live
        .par_iter()
        .map(|&(i, w)| {
            let c = key(w);
            let mut found: Vec<(usize, f64)> = Vec::new();
            for r in 0..=rings {
                for dz in -r..=r {
                    for dy in -r..=r {
                        for dx in -r..=r {
                            if dx.abs().max(dy.abs()).max(dz.abs()) != r {
                                continue;
                            }
                            let Some(list) = grid.get(&[c[0] + dx, c[1] + dy, c[2] + dz]) else { continue };
                            for &j in list {
                                let Some(Some(o)) = pts.get(j) else { continue };
                                if j == i {
                                    continue;
                                }
                                let d = ((o.world[0] - w[0]).powi(2) + (o.world[1] - w[1]).powi(2) + (o.world[2] - w[2]).powi(2)).sqrt();
                                if d <= max && d >= min {
                                    found.push((j, d));
                                }
                            }
                        }
                    }
                }
                // Anything in ring r + 1 is at least r · cell away.
                if found.len() >= k {
                    found.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
                    if found.get(k - 1).is_some_and(|e| e.1 <= r as f64 * cell) {
                        break;
                    }
                }
            }
            found.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
            found.truncate(k);
            found.into_iter().map(|(j, d)| (i.min(j), i.max(j), d)).collect()
        })
        .collect();
    let mut pairs: Vec<(usize, usize, f64)> = near.into_iter().flatten().collect();
    pairs.sort_by(|a, b| a.2.total_cmp(&b.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
    pairs.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    pairs.truncate(MAX_LINES);
    pairs
}

/// Triangles whose three sides are all pairs in `pairs` (each point's neighbours, i < j < k).
fn triangles(n: usize, pairs: &[(usize, usize, f64)]) -> Vec<(usize, usize, usize, f64)> {
    let mut adj: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    for &(i, j, d) in pairs {
        if let Some(a) = adj.get_mut(i) {
            a.push((j, d));
        }
    }
    for a in adj.iter_mut() {
        a.sort_by_key(|e| e.0);
    }
    let mut out = Vec::new();
    for (i, list) in adj.iter().enumerate() {
        for (x, &(j, dij)) in list.iter().enumerate() {
            for &(k, dik) in list.iter().skip(x + 1) {
                let Some(lj) = adj.get(j) else { continue };
                if let Ok(p) = lj.binary_search_by_key(&k, |e| e.0)
                    && let Some(&(_, djk)) = lj.get(p)
                {
                    out.push((i, j, k, dij.max(dik).max(djk)));
                    if out.len() >= MAX_FACETS {
                        return out;
                    }
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------------ rasterising

/// What to draw, in buffer pixels, colours premultiplied.
#[derive(Clone, Copy, Debug)]
enum Prim {
    Disc { c: [f32; 2], r: f32, soft: f32, col: Px },
    Line { a: [f32; 2], b: [f32; 2], wa: f32, wb: f32, ca: Px, cb: Px },
    Tri { p: [[f32; 2]; 3], col: [Px; 3] },
}

impl Prim {
    /// Bounding box (x0, y0, x1, y1), inclusive pixels.
    fn bbox(&self) -> [f32; 4] {
        match self {
            Prim::Disc { c, r, .. } => [c[0] - r - 1.0, c[1] - r - 1.0, c[0] + r + 1.0, c[1] + r + 1.0],
            Prim::Line { a, b, wa, wb, .. } => {
                let r = wa.max(*wb) * 0.5 + 1.0;
                [a[0].min(b[0]) - r, a[1].min(b[1]) - r, a[0].max(b[0]) + r, a[1].max(b[1]) + r]
            }
            Prim::Tri { p, .. } => {
                let xs = p.map(|q| q[0]);
                let ys = p.map(|q| q[1]);
                [
                    xs.iter().copied().fold(f32::MAX, f32::min) - 1.0,
                    ys.iter().copied().fold(f32::MAX, f32::min) - 1.0,
                    xs.iter().copied().fold(f32::MIN, f32::max) + 1.0,
                    ys.iter().copied().fold(f32::MIN, f32::max) + 1.0,
                ]
            }
        }
    }

    /// Coverage and colour at pixel centre `q`.
    fn sample(&self, q: [f32; 2]) -> Option<Px> {
        let scale = |c: Px, k: f32| c.map(|v| v * k);
        match *self {
            Prim::Disc { c, r, soft, col } => {
                let d = ((q[0] - c[0]).powi(2) + (q[1] - c[1]).powi(2)).sqrt();
                let rr = r.max(0.5);
                let hard = (rr + 0.5 - d).clamp(0.0, 1.0);
                let inner = rr * (1.0 - soft);
                let k = if soft > 0.0 && d > inner { hard * (1.0 - ((d - inner) / (rr - inner).max(1e-6)).clamp(0.0, 1.0)).powi(2) } else { hard };
                let k = k * (r / rr).min(1.0).powi(2);
                (k > 0.0).then(|| scale(col, k))
            }
            Prim::Line { a, b, wa, wb, ca, cb } => {
                let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                let l2 = dx * dx + dy * dy;
                let t = if l2 > 1e-12 { (((q[0] - a[0]) * dx + (q[1] - a[1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
                let (px, py) = (a[0] + dx * t, a[1] + dy * t);
                let d = ((q[0] - px).powi(2) + (q[1] - py).powi(2)).sqrt();
                let r = (wa + (wb - wa) * t) * 0.5;
                let rr = r.max(0.5);
                let k = (rr + 0.5 - d).clamp(0.0, 1.0) * (r / rr).min(1.0);
                (k > 0.0).then(|| {
                    let c: Px = [0, 1, 2, 3].map(|i| ca[i] + (cb[i] - ca[i]) * t);
                    scale(c, k)
                })
            }
            Prim::Tri { p, col } => {
                let e = |a: [f32; 2], b: [f32; 2]| (q[0] - a[0]) * (b[1] - a[1]) - (q[1] - a[1]) * (b[0] - a[0]);
                let area = e(p[0], p[1]) + e(p[1], p[2]) + e(p[2], p[0]);
                let full = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1]) - (p[1][1] - p[0][1]) * (p[2][0] - p[0][0]);
                if full.abs() < 1e-9 || !area.is_finite() {
                    return None;
                }
                let sgn = if full > 0.0 { -1.0 } else { 1.0 };
                // Signed distances to the three edges (positive inside).
                let dist = |a: [f32; 2], b: [f32; 2]| {
                    let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt().max(1e-9);
                    sgn * e(a, b) / len
                };
                let (d0, d1, d2) = (dist(p[1], p[2]), dist(p[2], p[0]), dist(p[0], p[1]));
                let k = (d0.min(d1).min(d2) + 0.5).clamp(0.0, 1.0);
                if k <= 0.0 {
                    return None;
                }
                // Barycentric weights from the edge functions.
                let w = [e(p[1], p[2]), e(p[2], p[0]), e(p[0], p[1])].map(|v| (v / -full).clamp(0.0, 1.0));
                let ws = (w[0] + w[1] + w[2]).max(1e-9);
                let c: Px = [0, 1, 2, 3].map(|i| (col[0][i] * w[0] + col[1][i] * w[1] + col[2][i] * w[2]) / ws);
                Some(scale(c, k))
            }
        }
    }
}

/// Rows per parallel band.
const BAND: usize = 16;

fn rasterize(w: usize, h: usize, prims: &[Prim], add: bool) -> Image {
    let mut img = Image::new(w as u32, h as u32);
    let bands = h.div_ceil(BAND);
    let mut bins: Vec<Vec<u32>> = vec![Vec::new(); bands];
    for (i, p) in prims.iter().enumerate() {
        let bb = p.bbox();
        if !bb.iter().all(|v| v.is_finite()) || bb[2] < 0.0 || bb[0] > w as f32 || bb[3] < 0.0 || bb[1] > h as f32 {
            continue;
        }
        let (b0, b1) = ((bb[1].max(0.0) as usize) / BAND, ((bb[3].min(h as f32 - 1.0)).max(0.0) as usize) / BAND);
        for bin in bins.iter_mut().take(b1 + 1).skip(b0) {
            bin.push(i as u32);
        }
    }
    img.data.par_chunks_mut(w * BAND).enumerate().for_each(|(bi, rows)| {
        let Some(list) = bins.get(bi) else { return };
        let y0 = bi * BAND;
        for &pi in list {
            let Some(p) = prims.get(pi as usize) else { continue };
            let bb = p.bbox();
            let (x0, x1) = (bb[0].max(0.0) as usize, (bb[2].min(w as f32 - 1.0)).max(0.0) as usize);
            let (ya, yb) = ((bb[1].max(y0 as f32)) as usize, (bb[3].min((y0 + BAND) as f32 - 1.0).min(h as f32 - 1.0)).max(0.0) as usize);
            for y in ya..=yb {
                let Some(row) = rows.get_mut((y - y0) * w..(y - y0 + 1) * w) else { continue };
                for (x, d) in row.iter_mut().enumerate().take(x1 + 1).skip(x0) {
                    let Some(s) = p.sample([x as f32 + 0.5, y as f32 + 0.5]) else { continue };
                    if add {
                        *d = [d[0] + s[0], d[1] + s[1], d[2] + s[2], (d[3] + s[3]).min(1.0)];
                    } else {
                        let k = 1.0 - s[3];
                        *d = [s[0] + d[0] * k, s[1] + d[1] * k, s[2] + d[2] * k, s[3] + d[3] * k];
                    }
                }
            }
        }
    });
    img
}

fn premul_rgb(c: [f32; 3], a: f32) -> Px {
    let a = a.clamp(0.0, 1.0);
    [c[0] * a, c[1] * a, c[2] * a, a]
}

fn plexus(ctx: &EffectCtx, b: Buf) -> Buf {
    let (w, h) = (b.img.width as usize, b.img.height as usize);
    if w == 0 || h == 0 || b.img.data.len() != w * h {
        return b;
    }
    let mut pts = geometry(ctx, &b);
    effectors(ctx, &mut pts);
    let seen = project(ctx, &b, &pts);
    let s = b.scale as f32;
    let pct = |id: &str, d: f64| (fv(ctx, id, d) / 100.0).clamp(0.0, 1.0) as f32;

    // (depth, primitive), drawn far to near.
    let mut prims: Vec<(f64, Prim)> = Vec::new();
    if ctx.params.b("plxFacets/on") {
        let max = fv(ctx, "plxFacets/maxDistance", 110.0).max(0.0);
        let op = pct("plxFacets/opacity", 20.0);
        let fall = ctx.params.b("plxFacets/opacityFalloff");
        let own = (!ctx.params.b("plxFacets/useVertexColors")).then(|| finite_color(ctx.params.color("plxFacets/color")));
        // Few neighbours per point keep facets from piling up into a blown-out mass.
        let pairs = connections(&seen, 0.0, max, FACET_NEIGHBOURS);
        for (i, j, k, d) in triangles(seen.len(), &pairs) {
            let (Some(Some(a)), Some(Some(bv)), Some(Some(c))) = (seen.get(i), seen.get(j), seen.get(k)) else { continue };
            let o = op * if fall { falloff(1, d / max.max(1e-9)) as f32 } else { 1.0 };
            let col = [a, bv, c].map(|v| premul_rgb(own.unwrap_or(v.color), o * v.alpha));
            let p = [a, bv, c].map(|v| [v.xy[0] as f32, v.xy[1] as f32]);
            prims.push(((a.depth + bv.depth + c.depth) / 3.0, Prim::Tri { p, col }));
        }
    }
    if ctx.params.b("plxLines/on") {
        let max = fv(ctx, "plxLines/maxDistance", 140.0).max(0.0);
        let min = fv(ctx, "plxLines/minDistance", 0.0).max(0.0);
        let k = fv(ctx, "plxLines/maxPoints", 10.0).round().clamp(1.0, 200.0) as usize;
        let thick = fv(ctx, "plxLines/thickness", 1.2).max(0.0) as f32 * s;
        let op = pct("plxLines/opacity", 80.0);
        let (ofall, tfall) = (ctx.params.e("plxLines/opacityFalloff"), ctx.params.e("plxLines/thicknessFalloff"));
        let own = (!ctx.params.b("plxLines/useVertexColors")).then(|| finite_color(ctx.params.color("plxLines/color")));
        let by_depth = ctx.params.b("plxPoints/scaleByDepth");
        for (i, j, d) in connections(&seen, min, max, k) {
            let (Some(Some(a)), Some(Some(bv))) = (seen.get(i), seen.get(j)) else { continue };
            let t = d / max.max(1e-9);
            let o = op * falloff(ofall, t) as f32;
            let th = thick * falloff(tfall, t) as f32;
            if o <= 0.0 || th <= 0.0 {
                continue;
            }
            let wd = |v: &Seen| if by_depth { th * v.scale as f32 } else { th };
            prims.push((
                (a.depth + bv.depth) * 0.5,
                Prim::Line {
                    a: [a.xy[0] as f32, a.xy[1] as f32],
                    b: [bv.xy[0] as f32, bv.xy[1] as f32],
                    wa: wd(a),
                    wb: wd(bv),
                    ca: premul_rgb(own.unwrap_or(a.color), o * a.alpha),
                    cb: premul_rgb(own.unwrap_or(bv.color), o * bv.alpha),
                },
            ));
        }
    }
    if ctx.params.b("plxPoints/on") {
        let size = fv(ctx, "plxPoints/size", 4.0).max(0.0) as f32 * s;
        let op = pct("plxPoints/opacity", 100.0);
        let soft = pct("plxPoints/softness", 30.0);
        let by_depth = ctx.params.b("plxPoints/scaleByDepth");
        if size > 0.0 && op > 0.0 {
            for v in seen.iter().flatten() {
                let r = 0.5 * size * if by_depth { v.scale as f32 } else { 1.0 };
                // Points sit just in front of the lines that meet them.
                prims.push((v.depth - 1e-6, Prim::Disc { c: [v.xy[0] as f32, v.xy[1] as f32], r, soft, col: premul_rgb(v.color, op * v.alpha) }));
            }
        }
    }
    prims.retain(|(d, p)| d.is_finite() && p.bbox().iter().all(|v| v.is_finite()) && (p.bbox()[2] - p.bbox()[0]) < 1e6);
    prims.sort_by(|a, b| b.0.total_cmp(&a.0));
    let list: Vec<Prim> = prims.into_iter().map(|(_, p)| p).collect();
    let add = ctx.params.e("blendMode") == 1;
    let drawn = rasterize(w, h, &list, add);

    let mut img = b.img;
    if ctx.params.b("composite") {
        for (d, s) in img.data.iter_mut().zip(&drawn.data) {
            if add {
                *d = [d[0] + s[0], d[1] + s[1], d[2] + s[2], (d[3] + s[3]).min(1.0)];
            } else {
                let k = 1.0 - s[3];
                *d = [s[0] + d[0] * k, s[1] + d[1] * k, s[2] + d[2] * k, s[3] + d[3] * k];
            }
        }
    } else {
        img = drawn;
    }
    Buf { img, offset: b.offset, scale: b.scale }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Params;

    fn ctx_params(vals: &[(&str, Value)], size: [f64; 2]) -> Params {
        let s = crate::find(ID).unwrap();
        let mut params = Params { values: s.params.iter().map(|p| (p.id.to_string(), crate::default_value(p, size))).collect() };
        for (k, v) in vals {
            params.values.insert(k.to_string(), v.clone());
        }
        params
    }

    fn run_at(vals: &[(&str, Value)], img: Image, time: f64) -> Image {
        let s = crate::find(ID).unwrap();
        let size = [img.width as f64, img.height as f64];
        let params = ctx_params(vals, size);
        let ctx = EffectCtx { params: &params, time, layer_size: size, seed: 1, adjustment: false, env: Default::default() };
        crate::apply(s, &ctx, Buf { img, offset: [0.0, 0.0], scale: 1.0 }).img
    }

    fn run(vals: &[(&str, Value)], img: Image) -> Image {
        run_at(vals, img, 0.0)
    }

    fn coverage(img: &Image) -> f32 {
        img.data.iter().map(|p| p[3]).sum()
    }

    fn only(extra: &[(&'static str, Value)]) -> Vec<(&'static str, Value)> {
        let mut v: Vec<(&'static str, Value)> = vec![("composite", Value::Bool(false)), ("plxNoise/amount", num(0.0)), ("plxTransform/spinY", num(0.0))];
        for (k, val) in extra {
            v.retain(|(x, _)| x != k);
            v.push((k, val.clone()));
        }
        v
    }

    #[test]
    fn noise_is_smooth_bounded_and_deterministic() {
        for i in 0..200 {
            let p = [i as f64 * 0.37, i as f64 * 0.11, i as f64 * 0.05];
            let v = noise3(p, 7);
            assert!((-1.0..=1.0).contains(&v));
            assert_eq!(v, noise3(p, 7));
            let near = noise3([p[0] + 1e-4, p[1], p[2]], 7);
            assert!((near - v).abs() < 1e-2);
        }
    }

    #[test]
    fn connections_respect_distance_and_nearest_count() {
        let pts: Vec<Option<Seen>> =
            (0..10).map(|i| Some(Seen { xy: [0.0; 2], depth: 1.0, scale: 1.0, world: [i as f64 * 10.0, 0.0, 0.0], color: [1.0; 3], alpha: 1.0 })).collect();
        // Neighbours 10 apart: within 15 only the chain links.
        let c = connections(&pts, 0.0, 15.0, 10);
        assert_eq!(c.len(), 9);
        assert!(c.iter().all(|&(i, j, d)| j == i + 1 && (d - 10.0).abs() < 1e-9));
        // Within 25 with k = 10: chain + skips.
        assert_eq!(connections(&pts, 0.0, 25.0, 10).len(), 9 + 8);
        // Min Distance drops the short links.
        assert_eq!(connections(&pts, 15.0, 25.0, 10).len(), 8);
        // k = 1: every point keeps one nearest (ties go to the lower index), deduplicated.
        let one = connections(&pts, 0.0, 25.0, 1);
        assert!(one.len() <= 10 && one.len() >= 5);
        // Triangles need all three sides.
        let tri = triangles(10, &connections(&pts, 0.0, 25.0, 10));
        assert_eq!(tri.len(), 8, "three in a row (0-1-2) count as a degenerate facet");
    }

    #[test]
    fn grid_lines_connect_neighbours_only() {
        let img = Image::new(200, 100);
        let vals = only(&[
            ("plxGeometry/source", Value::Enum(3)),
            ("plxGeometry/columns", num(5.0)),
            ("plxGeometry/rows", num(3.0)),
            ("plxGeometry/depth", num(0.0)),
            ("plxPoints/on", Value::Bool(false)),
            ("plxLines/maxDistance", num(55.0)),
            ("plxLines/opacityFalloff", Value::Enum(0)),
        ]);
        let out = run(&vals, img);
        // Columns are 50 apart, rows 50 apart: lines along the grid, none diagonal (70.7).
        let a = |x: i64, y: i64| out.get(x, y)[3];
        assert!(a(25, 50) > 0.3, "middle row, between the first two columns");
        assert!(a(50, 25) > 0.3, "second column, between the first two rows");
        assert!(a(25, 25) < 0.05, "no diagonal through the cell centre");
    }

    #[test]
    fn render_objects_switch_independently() {
        let img = Image::new(160, 90);
        let none = run(&only(&[("plxPoints/on", Value::Bool(false)), ("plxLines/on", Value::Bool(false))]), img.clone());
        assert_eq!(coverage(&none), 0.0);
        let points = run(&only(&[("plxLines/on", Value::Bool(false))]), img.clone());
        let lines = run(&only(&[("plxPoints/on", Value::Bool(false))]), img.clone());
        let both = run(&only(&[]), img.clone());
        assert!(coverage(&points) > 0.0 && coverage(&lines) > 0.0);
        assert!(coverage(&both) >= coverage(&points).max(coverage(&lines)) * 0.99);
        let facets = run(&only(&[("plxFacets/on", Value::Bool(true)), ("plxPoints/on", Value::Bool(false)), ("plxLines/on", Value::Bool(false))]), img);
        assert!(coverage(&facets) > 0.0, "facets fill triangles");
    }

    #[test]
    fn it_moves_over_time_and_is_deterministic() {
        let img = Image::new(128, 72);
        let a = run_at(&[("composite", Value::Bool(false))], img.clone(), 0.0);
        let again = run_at(&[("composite", Value::Bool(false))], img.clone(), 0.0);
        assert_eq!(a.data, again.data);
        let b = run_at(&[("composite", Value::Bool(false))], img.clone(), 1.0);
        assert_ne!(a.data, b.data, "noise and spin move it");
        // Without noise and spin it is still.
        let still = only(&[]);
        assert_eq!(run_at(&still, img.clone(), 0.0).data, run_at(&still, img, 2.0).data);
    }

    #[test]
    fn composite_keeps_the_layer_and_add_brightens() {
        let mut img = Image::new(64, 64);
        for p in img.data.iter_mut() {
            *p = [0.2, 0.2, 0.2, 1.0];
        }
        let over = run(&[], img.clone());
        assert!(over.data.iter().all(|p| p[3] >= 1.0 - 1e-6), "the layer stays under");
        let add = run(&[("blendMode", Value::Enum(1))], img.clone());
        assert!(add.data.iter().zip(&img.data).all(|(a, o)| a[0] >= o[0] - 1e-6));
        let alone = run(&[("composite", Value::Bool(false))], img);
        assert!(alone.data.iter().any(|p| p[3] == 0.0), "on its own the background is empty");
    }

    #[test]
    fn layer_pixels_put_points_on_bright_pixels() {
        let mut img = Image::new(100, 100);
        for y in 0..100 {
            for x in 60..100 {
                img.set(x, y, [1.0, 0.5, 0.0, 1.0]);
            }
        }
        let params = ctx_params(&[("plxGeometry/source", Value::Enum(4)), ("plxGeometry/count", num(300.0))], [100.0, 100.0]);
        let ctx = EffectCtx { params: &params, time: 0.0, layer_size: [100.0, 100.0], seed: 1, adjustment: false, env: Default::default() };
        let pts = geometry(&ctx, &Buf { img, offset: [0.0, 0.0], scale: 1.0 });
        assert_eq!(pts.len(), 300);
        assert!(pts.iter().all(|v| v.pos[0] >= 60.0 && v.pos[0] <= 100.0), "only on the bright half");
        assert!(pts.iter().all(|v| v.color == Some([1.0, 0.5, 0.0])));
    }

    #[test]
    fn masks_and_sphere_geometry() {
        let masks =
            [crate::MaskShape { name: "M".into(), points: vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]], closed: true, inverted: false }];
        let params = ctx_params(&[("plxGeometry/source", Value::Enum(5)), ("plxGeometry/count", num(40.0)), ("plxGeometry/depth", num(0.0))], [100.0, 100.0]);
        let env = crate::EffectEnv { masks: &masks, ..Default::default() };
        let ctx = EffectCtx { params: &params, time: 0.0, layer_size: [100.0, 100.0], seed: 1, adjustment: false, env };
        let pts = geometry(&ctx, &Buf { img: Image::new(100, 100), offset: [0.0, 0.0], scale: 1.0 });
        assert_eq!(pts.len(), 40);
        // Evenly spaced along the 400 px outline: one every 10 px, all on the square's edges.
        for v in &pts {
            let [x, y, _] = v.pos;
            let on_edge = x.abs() < 1e-6 || (x - 100.0).abs() < 1e-6 || y.abs() < 1e-6 || (y - 100.0).abs() < 1e-6;
            assert!(on_edge, "{x},{y}");
        }
        let params = ctx_params(&[("plxGeometry/source", Value::Enum(2)), ("plxGeometry/count", num(64.0))], [200.0, 200.0]);
        let ctx = EffectCtx { params: &params, time: 0.0, layer_size: [200.0, 200.0], seed: 1, adjustment: false, env: Default::default() };
        let pts = geometry(&ctx, &Buf { img: Image::new(200, 200), offset: [0.0, 0.0], scale: 1.0 });
        // On the ellipsoid with semi-axes 100, 100, 200.
        for v in &pts {
            let q = [(v.pos[0] - 100.0) / 100.0, (v.pos[1] - 100.0) / 100.0, v.pos[2] / 200.0];
            assert!(((q[0] * q[0] + q[1] * q[1] + q[2] * q[2]) - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn hostile_values_do_not_panic() {
        let img = Image::new(40, 30);
        for vals in [
            vec![("plxGeometry/count", num(1e12)), ("plxLines/maxDistance", num(1e300)), ("plxGeometry/depth", num(f64::NAN))],
            vec![("plxGeometry/center", Value::Vec2([f64::NAN, f64::INFINITY])), ("plxCamera/fov", num(f64::NAN)), ("plxPoints/size", num(1e30))],
            vec![("plxGeometry/source", Value::Enum(99)), ("plxTransform/scale", num(-1e9)), ("plxNoise/scale", num(0.0))],
            vec![("plxGeometry/source", Value::Enum(3)), ("plxGeometry/columns", num(1e9)), ("plxGeometry/rows", num(1e9)), ("plxGeometry/layersZ", num(1e9))],
            vec![("plxGeometry/centerZ", num(-1e9)), ("plxFacets/on", Value::Bool(true)), ("plxFacets/maxDistance", num(1e9))],
        ] {
            let out = run(&vals, img.clone());
            assert_eq!((out.width, out.height), (40, 30));
            assert!(out.data.iter().all(|p| p.iter().all(|v| v.is_finite())));
        }
        assert!(run(&[], Image::new(0, 0)).data.is_empty());
    }
}
