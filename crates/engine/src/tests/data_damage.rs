//! Data Damage (an original Stylize effect) and the data-damage text presets through the
//! session: applied to a text layer, animated by its envelope, and undone.

use effectcraft_render::{Image, RenderOpts};
use effectcraft_time::Tick;
use serde_json::json;

use crate::Session;

fn render(s: &Session, secs: f64) -> Image {
    let cid = s.active_comp_id().unwrap();
    s.render(cid, Tick::from_seconds_f64(secs), RenderOpts::default())
}

fn text_session() -> (Session, u64) {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "C", "width": 240, "height": 120, "duration": 4})).unwrap();
    let l = s.execute("layer.newText", json!({"text": "DAMAGE", "size": 48})).unwrap()["layer"].as_u64().unwrap();
    (s, l)
}

fn coverage(img: &Image) -> f32 {
    img.data.iter().map(|p| p[3]).sum()
}

#[test]
fn data_damage_glitches_text_on_and_undoes() {
    let (mut s, l) = text_session();
    let clean = render(&s, 2.0);
    let before = s.project.clone();
    s.execute("effect.apply", json!({"layers": [l], "effect": "Data Damage"})).unwrap();
    let held = render(&s, 0.5);
    assert_ne!(held.data, clean.data, "Hold damages the text");
    assert_eq!(held.data, render(&s, 0.5).data, "the same frame twice");
    // Glitch On from 0.5 s over 1 s: hidden first, clean after.
    for (k, v) in [("envelope", json!(1)), ("start", json!(0.5)), ("duration", json!(1.0))] {
        s.execute("prop.set", json!({"layer": l, "path": format!("effects/#1/{k}"), "value": v})).unwrap();
    }
    assert_eq!(coverage(&render(&s, 0.2)), 0.0, "hidden before Start");
    assert_eq!(render(&s, 2.0).data, clean.data, "clean once resolved");
    // The settings (Duration is already 1 s, so setting it is no step) and the effect itself.
    for _ in 0..3 {
        s.execute("edit.undo", json!({})).unwrap();
    }
    assert_eq!(*s.project, *before);
}

#[test]
fn data_damage_text_presets_scramble_then_settle() {
    let (mut s, l) = text_session();
    let clean = render(&s, 0.0);
    s.execute("layer.applyTextPreset", json!({"layer": l, "preset": "Data Damage On (Short)"})).unwrap();
    assert!(coverage(&render(&s, 0.0)) < 0.2 * coverage(&clean), "starts hidden");
    let mid = render(&s, 0.35);
    assert_ne!(mid.data, clean.data, "glitching halfway");
    assert_eq!(render(&s, 1.0).data, clean.data, "settled after about 18 frames");
}
