//! Retro Dither and Plexus (original Stylize / Generate effects) through the session: applied,
//! rendered, animated and undone.

use effectcraft_render::{Image, RenderOpts};
use effectcraft_time::Tick;
use serde_json::json;

use crate::Session;

fn render(s: &Session, secs: f64) -> Image {
    let cid = s.active_comp_id().unwrap();
    s.render(cid, Tick::from_seconds_f64(secs), RenderOpts::default())
}

fn session(w: u32, h: u32) -> (Session, u64) {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "C", "width": w, "height": h, "duration": 4})).unwrap();
    let l = s.execute("layer.newSolid", json!({"name": "S", "color": "#101828"})).unwrap()["layer"].as_u64().unwrap();
    (s, l)
}

#[test]
fn retro_dither_reduces_a_gradient_to_its_palette_and_undoes() {
    let (mut s, l) = session(96, 54);
    s.execute("effect.apply", json!({"layers": [l], "effect": "Gradient Ramp"})).unwrap();
    let before = s.project.clone();
    s.execute("effect.apply", json!({"layers": [l], "effect": "Retro Dither"})).unwrap();
    s.execute("prop.set", json!({"layer": l, "path": "effects/#2/palette", "value": 0})).unwrap();
    let img = render(&s, 0.0);
    assert!(img.data.iter().all(|p| p[..3].iter().all(|v| *v == 0.0 || (*v - 1.0).abs() < 1e-5)), "black and white only");
    let white = img.data.iter().filter(|p| p[0] > 0.5).count();
    assert!(white > img.data.len() / 5 && white < img.data.len() * 4 / 5, "a ramp keeps its tones as dot density");
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before);
}

#[test]
fn plexus_draws_a_moving_network_and_undoes() {
    let (mut s, l) = session(160, 90);
    let base = render(&s, 0.0);
    let before = s.project.clone();
    s.execute("effect.apply", json!({"layers": [l], "effect": "Plexus"})).unwrap();
    let a = render(&s, 0.0);
    let b = render(&s, 1.5);
    assert_ne!(a.data, base.data, "draws over the solid");
    assert_ne!(a.data, b.data, "moves over time");
    assert_eq!(a.data, render(&s, 0.0).data, "the same frame twice");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before);
}

/// Contact sheet for reviewing the looks by eye: `EC_LOOK_DUMP=dir cargo test -p
/// effectcraft-engine --lib retro_plexus_looks -- --ignored`.
#[test]
#[ignore = "writes images for review"]
fn retro_plexus_looks() {
    let Some(dir) = std::env::var_os("EC_LOOK_DUMP") else { return };
    let save = |name: &str, img: &Image| {
        let mut ppm = format!("P6\n{} {}\n255\n", img.width, img.height).into_bytes();
        ppm.extend(img.data.iter().flat_map(|p| (0..3).map(|i| (p[i].clamp(0.0, 1.0) * 255.0) as u8)));
        std::fs::write(std::path::Path::new(&dir).join(format!("{name}.ppm")), ppm).unwrap();
    };
    let dither: &[(&str, &[(&str, serde_json::Value)])] = &[
        ("dither_fs_ega", &[]),
        ("dither_bayer_gameboy_px4", &[("method", json!(3)), ("palette", json!(3)), ("pixelSize", json!(4))]),
        ("dither_atkinson_bw", &[("method", json!(10)), ("palette", json!(0))]),
        ("dither_c64_px3", &[("palette", json!(7)), ("pixelSize", json!(3)), ("method", json!(12))]),
        ("dither_graymap_gameboy_px2", &[("grayMap", json!(true)), ("palette", json!(3)), ("pixelSize", json!(2)), ("method", json!(10))]),
        ("dither_graymap_c64_bayer", &[("grayMap", json!(true)), ("palette", json!(7)), ("method", json!(3)), ("pixelSize", json!(3))]),
    ];
    for (name, vals) in dither {
        let (mut s, l) = session(480, 270);
        s.execute("effect.apply", json!({"layers": [l], "effect": "Fractal Noise"})).unwrap();
        s.execute("effect.apply", json!({"layers": [l], "effect": "Colorama"})).unwrap();
        s.execute("effect.apply", json!({"layers": [l], "effect": "Retro Dither"})).unwrap();
        for (k, v) in vals.iter() {
            s.execute("prop.set", json!({"layer": l, "path": format!("effects/#3/{k}"), "value": v})).unwrap();
        }
        save(name, &render(&s, 0.0));
    }
    let plexus: &[(&str, &[(&str, serde_json::Value)])] = &[
        ("plexus_default", &[]),
        ("plexus_facets_add", &[("plxFacets/on", json!(true)), ("blendMode", json!(1)), ("plxGeometry/count", json!(220))]),
        (
            "plexus_sphere",
            &[
                ("plxGeometry/source", json!(2)),
                ("plxGeometry/count", json!(260)),
                ("plxGeometry/width", json!(50)),
                ("plxGeometry/height", json!(85)),
                ("plxGeometry/depth", json!(230)),
                ("plxLines/maxDistance", json!(60)),
            ],
        ),
        (
            "plexus_grid",
            &[
                ("plxGeometry/source", json!(3)),
                ("plxGeometry/columns", json!(16)),
                ("plxGeometry/rows", json!(9)),
                ("plxGeometry/layersZ", json!(3)),
                ("plxLines/maxDistance", json!(70)),
                ("plxTransform/rotationX", json!(35)),
            ],
        ),
        (
            "plexus_field_container",
            &[
                ("plxGeometry/count", json!(500)),
                ("plxNoise/amount", json!(160)),
                ("plxContainer/shape", json!(2)),
                ("plxContainer/scale", json!(70)),
                ("plxField/on", json!(true)),
                ("plxField/center", json!([240, 135])),
                ("plxField/radius", json!(110)),
                ("plxField/strength", json!(-45)),
                ("plxField/scale", json!(260)),
                ("plxField/colorAmount", json!(100)),
                ("plxLines/maxDistance", json!(55)),
            ],
        ),
        (
            "plexus_beams_dof",
            &[
                ("plxGeometry/source", json!(1)),
                ("plxGeometry/count", json!(220)),
                ("plxGeometry/depth", json!(900)),
                ("plxCamera/dof", json!(true)),
                ("plxCamera/focus", json!(650)),
                ("plxCamera/aperture", json!(9)),
                ("plxBeams/on", json!(true)),
                ("plxBeams/every", json!(6)),
                ("plxBeams/target", json!([240, 135])),
                ("plxBeams/opacity", json!(35)),
                ("plxLines/colorByLength", json!(true)),
            ],
        ),
        (
            "plexus_facets_cull_blur",
            &[
                ("plxGeometry/source", json!(2)),
                ("plxGeometry/count", json!(300)),
                ("plxGeometry/width", json!(55)),
                ("plxGeometry/height", json!(95)),
                ("plxGeometry/depth", json!(260)),
                ("plxFacets/on", json!(true)),
                ("plxFacets/opacity", json!(45)),
                ("plxFacets/cull", json!(1)),
                ("plxFacets/colorBySize", json!(true)),
                ("plxLines/maxDistance", json!(55)),
                ("plxTransform/spinY", json!(90)),
                ("plxMotionBlur/mode", json!(2)),
                ("plxMotionBlur/angle", json!(270)),
            ],
        ),
    ];
    for (name, vals) in plexus {
        let (mut s, l) = session(480, 270);
        s.execute("effect.apply", json!({"layers": [l], "effect": "Plexus"})).unwrap();
        for (k, v) in vals.iter() {
            s.execute("prop.set", json!({"layer": l, "path": format!("effects/#1/{k}"), "value": v})).unwrap();
        }
        save(name, &render(&s, 1.0));
    }
    // Data Damage on a text layer: held damage, mid Glitch On, and the text preset.
    let damage: &[(&str, f64, &[(&str, serde_json::Value)])] = &[
        ("damage_hold", 0.4, &[("damage", json!(80))]),
        ("damage_glitch_on_mid", 0.55, &[("envelope", json!(1)), ("damage", json!(100))]),
        ("damage_heavy_split", 0.9, &[("damage", json!(100)), ("rgbSplit", json!(14)), ("pixelate", json!(60)), ("blockSize", json!(36))]),
    ];
    for (name, t, vals) in damage {
        let mut s = Session::default();
        s.execute("comp.new", json!({"name": "C", "width": 480, "height": 270, "duration": 4})).unwrap();
        s.execute("layer.newSolid", json!({"name": "BG", "color": "#0c1018"})).unwrap();
        let l = s.execute("layer.newText", json!({"text": "DATA DAMAGE", "size": 64})).unwrap()["layer"].as_u64().unwrap();
        s.execute("effect.apply", json!({"layers": [l], "effect": "Data Damage"})).unwrap();
        for (k, v) in vals.iter() {
            s.execute("prop.set", json!({"layer": l, "path": format!("effects/#1/{k}"), "value": v})).unwrap();
        }
        save(name, &render(&s, *t));
    }
    for (name, preset, t) in [("text_data_damage_on_mid", "dataDamageOn", 0.45), ("text_data_damage_off_mid", "dataDamageOff", 0.4)] {
        let mut s = Session::default();
        s.execute("comp.new", json!({"name": "C", "width": 480, "height": 270, "duration": 4})).unwrap();
        s.execute("layer.newSolid", json!({"name": "BG", "color": "#0c1018"})).unwrap();
        let l = s.execute("layer.newText", json!({"text": "DATA DAMAGE", "size": 64})).unwrap()["layer"].as_u64().unwrap();
        s.execute("layer.applyTextPreset", json!({"layer": l, "preset": preset})).unwrap();
        save(name, &render(&s, t));
    }
}
