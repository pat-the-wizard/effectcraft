//! Pixel Sort (an original Stylize effect) through the session: applied after a generator it
//! sorts the rendered rows, a Constraint Layer (hidden, as a matte source) limits where it
//! sorts, and it undoes. Like every layer parameter, the Constraint Layer's source is centred
//! on the effect's layer, whatever its own transform.

use effectcraft_keyframe::Value as KV;
use effectcraft_project::LayerId;
use effectcraft_render::{Image, RenderOpts};
use effectcraft_time::Tick;
use serde_json::json;

use crate::Session;

const W: u32 = 160;
const H: u32 = 90;

fn luma(p: [f32; 4]) -> f32 {
    let a = p[3].max(1e-6);
    effectcraft_color::luminance(p[0] / a, p[1] / a, p[2] / a)
}

fn row(img: &Image, y: u32, xs: std::ops::Range<u32>) -> Vec<f32> {
    xs.map(|x| luma(img.get(i64::from(x), i64::from(y)))).collect()
}

fn ascending(v: &[f32]) -> bool {
    v.windows(2).all(|p| p[0] <= p[1] + 1e-6)
}

fn render(s: &Session) -> Image {
    let cid = s.active_comp_id().unwrap();
    s.render(cid, Tick::ZERO, RenderOpts::default())
}

#[test]
fn pixel_sort_sorts_rendered_rows_inside_the_constraint_and_undoes() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "C", "width": W, "height": H, "duration": 2})).unwrap();
    // The matte: a hidden white solid half as wide, so it covers the middle half (x 40..120).
    let matte = s.execute("layer.newSolid", json!({"name": "Matte", "color": "#ffffff", "width": W / 2, "height": H})).unwrap()["layer"].as_u64().unwrap();
    let cid = s.active_comp_id().unwrap();
    s.edit("Place matte", None, |proj, _| {
        let l = proj.comp_mut(cid).and_then(|c| c.layer_mut(LayerId(matte))).ok_or_else(|| crate::EngineError::Other("no matte".into()))?;
        l.switches.video = false;
        Ok(())
    })
    .unwrap();
    let bg = s.execute("layer.newSolid", json!({"name": "Noise", "color": "#808080", "width": W, "height": H})).unwrap()["layer"].as_u64().unwrap();
    s.execute("effect.apply", json!({"layers": [bg], "effect": "Fractal Noise"})).unwrap();
    let noise = render(&s);
    assert!(!ascending(&row(&noise, 40, 0..W)), "the noise isn't sorted to begin with");

    let before = s.project.clone();
    s.execute("effect.apply", json!({"layers": [bg], "effect": "Pixel Sort"})).unwrap();
    s.execute("prop.set", json!({"layer": bg, "path": "effects/#2/lowerThreshold", "value": 0})).unwrap();
    let sorted = render(&s);
    for y in [0, 31, 89] {
        assert!(ascending(&row(&sorted, y, 0..W)), "row {y} ascends left to right");
    }

    // Only the middle half under the matte is sorted; the sides keep the noise.
    s.edit("Constrain", None, |proj, _| {
        let l = proj.comp_mut(cid).and_then(|c| c.layer_mut(LayerId(bg))).ok_or_else(|| crate::EngineError::Other("no layer".into()))?;
        if let Some(p) = l.props.prop_mut("effects/#2/constraintLayer") {
            p.value = KV::Layer(Some(matte));
        }
        Ok(())
    })
    .unwrap();
    let half = render(&s);
    for y in [5, 45, 80] {
        assert!(ascending(&row(&half, y, W / 4..3 * W / 4)), "row {y}: the middle is sorted");
        assert_eq!(row(&half, y, 0..W / 4), row(&noise, y, 0..W / 4), "row {y}: the left quarter is untouched");
        assert_eq!(row(&half, y, 3 * W / 4..W), row(&noise, y, 3 * W / 4..W), "row {y}: the right quarter is untouched");
    }

    // Down (180°) sorts the columns instead.
    s.execute("prop.set", json!({"layer": bg, "path": "effects/#2/direction", "value": 180})).unwrap();
    let down = render(&s);
    for x in [45, 80, 115] {
        let col: Vec<f32> = (0..H).map(|y| luma(down.get(i64::from(x), i64::from(y)))).collect();
        assert!(ascending(&col), "column {x} ascends downwards");
    }
    for x in [3, 150] {
        let col: Vec<f32> = (0..H).map(|y| luma(down.get(i64::from(x), i64::from(y)))).collect();
        let orig: Vec<f32> = (0..H).map(|y| luma(noise.get(i64::from(x), i64::from(y)))).collect();
        assert_eq!(col, orig, "column {x} is outside the matte");
    }

    for _ in 0..4 {
        s.execute("edit.undo", json!({})).unwrap();
    }
    assert_eq!(*s.project, *before, "undo removes Pixel Sort");
}

/// Contact sheet for reviewing the look by eye: `EC_PIXELSORT_DUMP=dir cargo test -p
/// effectcraft-engine --lib pixel_sort_looks -- --ignored`.
#[test]
#[ignore = "writes images for review"]
fn pixel_sort_looks() {
    let Some(dir) = std::env::var_os("EC_PIXELSORT_DUMP") else { return };
    let variants: &[(&str, &[(&str, serde_json::Value)])] = &[
        ("off", &[]),
        ("default", &[("enabled", json!(true))]),
        ("down_random_length", &[("direction", json!(180)), ("maxLength", json!(120)), ("randomLength", json!(70))]),
        ("diagonal_shadows", &[("direction", json!(135)), ("invertSpans", json!(true)), ("lowerThreshold", json!(45))]),
        ("hue_gradient_feather", &[("sortBy", json!(1)), ("fill", json!(1)), ("feather", json!(12)), ("maxLength", json!(60))]),
        ("random_reverse", &[("sortBy", json!(8)), ("lowerThreshold", json!(55)), ("direction", json!(0))]),
    ];
    for (name, vals) in variants {
        let mut s = Session::default();
        s.execute("comp.new", json!({"name": "C", "width": 480, "height": 270, "duration": 2})).unwrap();
        let l = s.execute("layer.newSolid", json!({"name": "S", "color": "#808080"})).unwrap()["layer"].as_u64().unwrap();
        s.execute("effect.apply", json!({"layers": [l], "effect": "Fractal Noise"})).unwrap();
        s.execute("effect.apply", json!({"layers": [l], "effect": "Colorama"})).unwrap();
        if *name != "off" {
            s.execute("effect.apply", json!({"layers": [l], "effect": "Pixel Sort"})).unwrap();
            for (k, v) in vals.iter().filter(|(k, _)| *k != "enabled") {
                s.execute("prop.set", json!({"layer": l, "path": format!("effects/#3/{k}"), "value": v})).unwrap();
            }
        }
        let img = render(&s);
        let mut ppm = format!("P6\n{} {}\n255\n", img.width, img.height).into_bytes();
        ppm.extend(img.data.iter().flat_map(|p| (0..3).map(|i| (p[i].clamp(0.0, 1.0) * 255.0) as u8)));
        std::fs::write(std::path::Path::new(&dir).join(format!("{name}.ppm")), ppm).unwrap();
    }
}
