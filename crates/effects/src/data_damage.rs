//! Data Damage: an original EffectCraft effect (Stylize), not an After Effects one. It makes a
//! layer look like corrupted digital video: bands of the picture slip sideways, blocks turn
//! into big pixels or drop out, the colour channels split, digital noise and scanlines cover
//! it, and the pattern jumps several times a second. Its envelope can glitch a layer on (it
//! flickers into view and the damage resolves), glitch it off, burst at random, or hold.
//!
//! Written from general knowledge of digital video errors and from the published description
//! of glitch "animated typeface" packs (letters that glitch on with noise, grain, pixelation and
//! screen errors, in a long and a short version); those packs are pre-rendered footage, so
//! nothing of them is used here: the behaviour is built as a procedural effect that works on
//! any layer and any font. The code, parameters and maths are our own.
//!
//! * **Pattern.** Time is cut into steps (Changes per Second); every step re-rolls a hash for
//!   each horizontal band (Band Height) and each block (Block Size), seeded by Random Seed.
//! * **Damage** scales every part: how many bands slip and how far (Displacement), how many
//!   blocks pixelate (Pixelate) or drop out (Dropout), the channel split (RGB Split), the noise
//!   (Digital Noise) and the scanlines.
//! * **Envelope.** Hold keeps Damage constant. Glitch On hides the layer before Start, then over
//!   Duration it flickers into view more and more often while the damage dies away. Glitch Off
//!   is the reverse. Bursts damage the layer in random short bursts (Bursts per Second).

use effectcraft_keyframe::Value;
use effectcraft_raster::{Image, Px};
use rayon::prelude::*;

use crate::{Buf, EffectCtx, EffectSpec, num, p, popup, slider};

pub const ID: &str = "ec.stylize.datadamage";

const ENVELOPES: &[&str] = &["Hold", "Glitch On", "Glitch Off", "Bursts"];

/// Longest block / band / displacement in layer pixels.
const MAX_PX: f64 = 4096.0;

pub fn specs() -> Vec<EffectSpec> {
    vec![EffectSpec {
        id: ID,
        name: "Data Damage",
        category: "Stylize",
        params: vec![
            p("damage", "Damage", num(60.0), slider(0.0, 100.0, 0.0, 100.0, 1)),
            p("envelope", "Envelope", Value::Enum(0), popup(ENVELOPES)),
            p("start", "Start (s)", num(0.0), slider(-100_000.0, 100_000.0, 0.0, 10.0, 2)),
            p("duration", "Duration (s)", num(1.0), slider(0.0, 100_000.0, 0.0, 5.0, 2)),
            p("bursts", "Bursts per Second", num(1.5), slider(0.0, 100.0, 0.0, 10.0, 2)),
            p("rate", "Changes per Second", num(15.0), slider(0.0, 1000.0, 0.0, 60.0, 1)),
            p("bandHeight", "Band Height", num(14.0), slider(1.0, MAX_PX, 1.0, 100.0, 1)),
            p("displacement", "Displacement", num(40.0), slider(0.0, MAX_PX, 0.0, 300.0, 1)),
            p("blockSize", "Block Size", num(24.0), slider(2.0, MAX_PX, 2.0, 200.0, 1)),
            p("pixelate", "Pixelate", num(35.0), slider(0.0, 100.0, 0.0, 100.0, 1)),
            p("dropout", "Dropout", num(12.0), slider(0.0, 100.0, 0.0, 100.0, 1)),
            p("rgbSplit", "RGB Split", num(6.0), slider(0.0, MAX_PX, 0.0, 50.0, 1)),
            p("noise", "Digital Noise", num(30.0), slider(0.0, 100.0, 0.0, 100.0, 1)),
            p("scanlines", "Scanlines", num(20.0), slider(0.0, 100.0, 0.0, 100.0, 1)),
            p("seed", "Random Seed", num(0.0), slider(0.0, 10_000.0, 0.0, 1000.0, 0)),
        ],
        render: data_damage,
        gpu: false,
        float: true,
    }]
}

fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Uniform 0..1 from three integers and a seed.
fn h3(a: i64, b: i64, c: u64, seed: u64) -> f64 {
    let z = mix64(seed ^ mix64((a as u64) ^ mix64((b as u64).wrapping_mul(0x632B_E59B_D9B4_E019) ^ c)));
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// The evaluated parameters; lengths in buffer pixels.
#[derive(Clone, Copy, Debug)]
struct Look {
    /// Damage after the envelope, 0..1.
    damage: f64,
    /// Whether the layer shows at all this step (Glitch On / Off flicker).
    visible: bool,
    step: i64,
    seed: u64,
    band: f64,
    shift: f64,
    block: f64,
    pixelate: f64,
    dropout: f64,
    split: f64,
    noise: f32,
    scan: f32,
}

fn smooth(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn look(ctx: &EffectCtx, scale: f64) -> Look {
    let f = |id: &str, d: f64| {
        let v = ctx.params.f(id);
        if v.is_finite() { v } else { d }
    };
    let t = if ctx.time.is_finite() { ctx.time.clamp(-1e7, 1e7) } else { 0.0 };
    let rate = f("rate", 15.0).clamp(0.0, 1000.0);
    let step = if rate > 0.0 { (t * rate).floor() as i64 } else { 0 };
    let seed = (f("seed", 0.0).clamp(0.0, 1e9) as u64) ^ (u64::from(ctx.seed) << 32);
    let base = (f("damage", 60.0) / 100.0).clamp(0.0, 1.0);
    let start = f("start", 0.0).clamp(-1e7, 1e7);
    let dur = f("duration", 1.0).clamp(0.0, 1e7);
    // Progress through the envelope's window (a zero duration is a cut).
    let u = if dur > 0.0 {
        (t - start) / dur
    } else if t >= start {
        1.0
    } else {
        -1.0
    };
    let flick = h3(step, -7, 0x666C_6963, seed);
    let (damage, visible) = match ctx.params.e("envelope") {
        1 => {
            if u < 0.0 {
                (0.0, false)
            } else if u >= 1.0 {
                (0.0, true)
            } else {
                // More and more steps show the layer; the damage resolves as it settles.
                (base * (1.0 - smooth(u)).max(0.15), flick < smooth(u).sqrt().max(0.2))
            }
        }
        2 => {
            if u < 0.0 {
                (0.0, true)
            } else if u >= 1.0 {
                (0.0, false)
            } else {
                (base * smooth(u).max(0.15), flick < (1.0 - smooth(u)).sqrt().max(0.2))
            }
        }
        3 => {
            let per = f("bursts", 1.5).clamp(0.0, 100.0);
            // Quarter-second slots, each damaged with chance per / 4: `per` bursts a second on
            // average.
            let slot = (t * 4.0).floor() as i64;
            let on = per > 0.0 && h3(slot, 11, 0x6275_7273, seed) < (per / 4.0).min(1.0);
            (if on { base } else { 0.0 }, true)
        }
        _ => (base, true),
    };
    let len = |id: &str, d: f64, lo: f64| (f(id, d).clamp(lo, MAX_PX) * scale).max(1e-3);
    let pc = |id: &str, d: f64| (f(id, d) / 100.0).clamp(0.0, 1.0);
    Look {
        damage,
        visible,
        step,
        seed,
        band: len("bandHeight", 14.0, 1.0),
        shift: f("displacement", 40.0).clamp(0.0, MAX_PX) * scale,
        block: len("blockSize", 24.0, 2.0),
        pixelate: pc("pixelate", 35.0),
        dropout: pc("dropout", 12.0),
        split: f("rgbSplit", 6.0).clamp(0.0, MAX_PX) * scale,
        noise: pc("noise", 30.0) as f32,
        scan: pc("scanlines", 20.0) as f32,
    }
}

/// The damaged pixel at buffer pixel (`x`, `y`); `lx`, `ly` are layer-aligned coordinates
/// (buffer pixels from the layer origin) so the pattern doesn't swim when the buffer grows.
fn damaged(src: &Image, lk: &Look, x: i64, y: i64, lx: f64, ly: f64) -> Px {
    let d = lk.damage;
    let s = lk.step as u64;
    // Band slip.
    let band = (ly / lk.band).floor() as i64;
    let hb = h3(band, 1, s, lk.seed);
    let mut sx = 0.0;
    if hb < d * 0.6 {
        sx = (h3(band, 2, s, lk.seed) * 2.0 - 1.0) * lk.shift * d;
    }
    // Blocks: drop out, pixelate.
    let (bx, by) = ((lx / lk.block).floor() as i64, (ly / lk.block).floor() as i64);
    if h3(bx, by ^ 0x55, s, lk.seed) < lk.dropout * d * 0.5 {
        return [0.0; 4];
    }
    let (mut qx, mut qy) = (x as f64 + sx, y as f64);
    if h3(bx, by ^ 0x77, s, lk.seed) < lk.pixelate * d {
        // Big pixels: a third of a block, aligned to the block.
        let cell = (lk.block / 3.0).max(1.0);
        qx = (((lx + sx) / cell).floor() + 0.5) * cell + (x as f64 - lx);
        qy = ((ly / cell).floor() + 0.5) * cell + (y as f64 - ly);
    }
    let at = |px: f64, py: f64| -> Px { if px.is_finite() && py.is_finite() { src.get(px.floor() as i64, py.floor() as i64) } else { [0.0; 4] } };
    let split = lk.split * d;
    let mid = at(qx, qy);
    let mut out = if split >= 0.5 {
        let (r, b) = (at(qx + split, qy), at(qx - split, qy));
        // Each channel keeps its own coverage; alpha is the widest of the three.
        [r[0], mid[1], b[2], r[3].max(mid[3]).max(b[3])]
    } else {
        mid
    };
    if out[3] > 0.0 {
        if lk.noise > 0.0 {
            let n = (h3(x, y, s ^ 0x6E6F_6973, lk.seed) as f32 - 0.5) * 2.0 * lk.noise * d as f32;
            let na = n * out[3];
            for c in out.iter_mut().take(3) {
                *c = (*c + na).max(0.0);
            }
        }
        if lk.scan > 0.0 && (ly.floor() as i64).rem_euclid(2) == 1 {
            let k = 1.0 - lk.scan * d as f32 * 0.6;
            for c in out.iter_mut().take(3) {
                *c *= k;
            }
        }
    }
    out.map(|v| if v.is_finite() { v } else { 0.0 })
}

fn data_damage(ctx: &EffectCtx, b: Buf) -> Buf {
    let (w, h) = (b.img.width as usize, b.img.height as usize);
    if w == 0 || h == 0 || b.img.data.len() != w * h {
        return b;
    }
    let lk = look(ctx, b.scale);
    if !lk.visible {
        return Buf { img: Image::new(b.img.width, b.img.height), offset: b.offset, scale: b.scale };
    }
    if lk.damage <= 0.0 {
        return b;
    }
    let src = &b.img;
    let (ox, oy) = (b.offset[0], b.offset[1]);
    let mut data = vec![[0.0f32; 4]; w * h];
    data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, d) in row.iter_mut().enumerate() {
            *d = damaged(src, &lk, x as i64, y as i64, x as f64 - ox, y as f64 - oy);
        }
    });
    Buf { img: Image { width: b.img.width, height: b.img.height, data }, offset: b.offset, scale: b.scale }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Params;

    fn run(vals: &[(&str, Value)], img: Image, time: f64) -> Image {
        let s = crate::find(ID).unwrap();
        let mut params = Params { values: s.params.iter().map(|p| (p.id.to_string(), p.default.clone())).collect() };
        for (k, v) in vals {
            params.values.insert(k.to_string(), v.clone());
        }
        let ctx = EffectCtx { params: &params, time, layer_size: [img.width as f64, img.height as f64], seed: 3, adjustment: false, env: Default::default() };
        crate::apply(s, &ctx, Buf { img, offset: [0.0, 0.0], scale: 1.0 }).img
    }

    fn card(w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = ((x / 4 + y / 4) % 2) as f32 * 0.6 + 0.2;
                img.set(x, y, [v, x as f32 / w as f32, 1.0 - v, 1.0]);
            }
        }
        img
    }

    fn alpha(img: &Image) -> f32 {
        img.data.iter().map(|p| p[3]).sum()
    }

    #[test]
    fn zero_damage_is_a_no_op_and_damage_changes_the_picture() {
        let img = card(64, 48);
        assert_eq!(run(&[("damage", num(0.0))], img.clone(), 0.3).data, img.data);
        let out = run(&[("damage", num(100.0))], img.clone(), 0.3);
        assert_ne!(out.data, img.data);
        assert_eq!((out.width, out.height), (64, 48));
    }

    #[test]
    fn pattern_holds_within_a_step_and_changes_between_steps() {
        let img = card(64, 48);
        let vals = [("damage", num(100.0)), ("rate", num(10.0))];
        let a = run(&vals, img.clone(), 0.21);
        let b = run(&vals, img.clone(), 0.29);
        let c = run(&vals, img, 0.31);
        assert_eq!(a.data, b.data, "same step");
        assert_ne!(b.data, c.data, "next step");
    }

    #[test]
    fn glitch_on_hides_then_resolves_and_glitch_off_ends_hidden() {
        let img = card(64, 48);
        let on = [("envelope", Value::Enum(1)), ("start", num(1.0)), ("duration", num(1.0))];
        assert_eq!(alpha(&run(&on, img.clone(), 0.5)), 0.0, "hidden before Start");
        assert_eq!(run(&on, img.clone(), 2.5).data, img.data, "clean after the window");
        // During the window some steps show the layer, damaged.
        let shown = (0..30).map(|i| run(&on, img.clone(), 1.0 + i as f64 / 30.0)).filter(|o| alpha(o) > 0.0).count();
        assert!(shown > 5 && shown < 30, "flickers into view: {shown}");
        let off = [("envelope", Value::Enum(2)), ("start", num(1.0)), ("duration", num(1.0))];
        assert_eq!(run(&off, img.clone(), 0.5).data, img.data, "clean before Start");
        assert_eq!(alpha(&run(&off, img, 2.5)), 0.0, "gone after the window");
    }

    #[test]
    fn dropout_only_removes_and_split_moves_channels_apart() {
        // A white bar on transparent: red reads from the right and blue from the left, so red
        // spreads further left and blue further right.
        let mut img = Image::new(40, 8);
        for y in 0..8 {
            for x in 15..25 {
                img.set(x, y, [1.0, 1.0, 1.0, 1.0]);
            }
        }
        let vals = [
            ("damage", num(100.0)),
            ("displacement", num(0.0)),
            ("pixelate", num(0.0)),
            ("dropout", num(0.0)),
            ("noise", num(0.0)),
            ("scanlines", num(0.0)),
            ("rgbSplit", num(3.0)),
        ];
        let out = run(&vals, img.clone(), 0.0);
        let px = |x: u32| out.get(i64::from(x), 2);
        assert_eq!((px(13)[0], px(13)[1], px(13)[2]), (1.0, 0.0, 0.0), "red only, 2 px left of the bar");
        assert_eq!((px(26)[0], px(26)[1], px(26)[2]), (0.0, 0.0, 1.0), "blue only, 2 px right of it");
        assert_eq!(px(13)[3], 1.0, "coverage is the widest channel's");
        // Dropout alone never adds coverage.
        let d = run(
            &[("damage", num(100.0)), ("dropout", num(100.0)), ("rgbSplit", num(0.0)), ("displacement", num(0.0)), ("pixelate", num(0.0))],
            img.clone(),
            0.0,
        );
        assert!(alpha(&d) < alpha(&img));
    }

    #[test]
    fn bursts_switch_damage_on_and_off() {
        let img = card(32, 32);
        let vals = [("envelope", Value::Enum(3)), ("bursts", num(2.0)), ("damage", num(100.0))];
        let changed = (0..40).filter(|i| run(&vals, img.clone(), *i as f64 * 0.25).data != img.data).count();
        assert!(changed > 0 && changed < 40, "{changed}");
    }

    /// Hostile values render without a panic and keep the buffer size.
    #[test]
    fn hostile_values_do_not_panic() {
        let mut img = card(17, 9);
        img.set(3, 3, [f32::NAN, f32::INFINITY, -1.0, 1.0]);
        for vals in [
            vec![("damage", num(f64::NAN)), ("rate", num(f64::INFINITY)), ("bandHeight", num(-5.0))],
            vec![("damage", num(1e300)), ("displacement", num(1e300)), ("blockSize", num(0.0)), ("rgbSplit", num(f64::NAN))],
            vec![("envelope", Value::Enum(99)), ("start", num(f64::NAN)), ("duration", num(-1.0)), ("seed", num(-1e30))],
            vec![("envelope", Value::Enum(3)), ("bursts", num(1e9)), ("damage", num(100.0))],
        ] {
            for t in [f64::NAN, -1e300, 0.0, 1e300] {
                let out = run(&vals, img.clone(), t);
                assert_eq!((out.width, out.height), (17, 9));
                assert!(out.data.iter().flatten().all(|v| v.is_finite()));
            }
        }
        assert!(run(&[], Image::new(0, 0), 0.0).data.is_empty());
    }
}
