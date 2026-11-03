//! The text animation presets through the wired session (with the expression engine): every
//! Loops preset keeps moving with no expression errors and renders the same frame twice, and
//! every preset renders finite pixels at the start, middle and end of its animation.

use effectcraft_engine::render::RenderOpts;
use effectcraft_time::Tick;
use serde_json::json;

fn frame(s: &effectcraft_engine::Session, secs: f64) -> Vec<[f32; 4]> {
    let Some(cid) = s.active_comp_id() else { panic!("no active comp") };
    s.render(cid, Tick::from_seconds_f64(secs), RenderOpts { scale: 0.5, ..Default::default() }).data
}

fn diff(a: &[[f32; 4]], b: &[[f32; 4]]) -> f64 {
    a.iter().zip(b).map(|(p, q)| (0..4).map(|i| (p[i] - q[i]).abs() as f64).sum::<f64>()).sum::<f64>() / a.len().max(1) as f64
}

#[test]
fn every_text_preset_renders_and_loops_keep_moving() {
    let catalog = effectcraft_engine::text_preset_categories();
    assert!(catalog.len() >= 30, "{}", catalog.len());
    for (id, name, cat) in catalog {
        let mut s = effectcraft_host::session();
        s.execute("comp.new", json!({"name": "T", "width": 640, "height": 360, "frameRate": 30, "duration": 6})).unwrap();
        let t = s.execute("layer.newText", json!({"text": "Motion Graphics\nin EffectCraft", "size": 56})).unwrap()["layer"].as_u64().unwrap();
        let base = frame(&s, 0.0);
        s.execute("layer.applyTextPreset", json!({"layer": t, "preset": id})).unwrap_or_else(|e| panic!("{name}: {e}"));
        let times = [0.0, 0.4, 0.9, 1.6, 3.0];
        let frames: Vec<_> = times.iter().map(|&secs| frame(&s, secs)).collect();
        for (secs, f) in times.iter().zip(&frames) {
            assert!(f.iter().all(|p| p.iter().all(|c| c.is_finite())), "{id} @ {secs}: a pixel is not finite");
            let errors = s.execute("expr.errors", json!({"time": secs})).unwrap();
            assert_eq!(errors["count"], 0, "{id} @ {secs}: {}", errors["text"]);
            if let Some(dir) = std::env::var_os("EC_PRESET_DUMP") {
                // Contact-sheet helper for reviewing the presets by eye (not part of the check).
                let mut ppm = b"P6\n320 180\n255\n".to_vec();
                ppm.extend(f.iter().flat_map(|p| (0..3).map(|i| ((p[i] * p[3] + 0.08 * (1.0 - p[3])).clamp(0.0, 1.0) * 255.0) as u8)));
                let _ = std::fs::write(std::path::Path::new(&dir).join(format!("{cat}_{id}_{secs}.ppm")), ppm);
            }
        }
        if cat == "Loops" {
            // Moving between any two sampled times, not stuck on the untouched text.
            for w in frames.windows(2) {
                assert!(diff(&w[0], &w[1]) > 1e-4, "{id}: keeps moving");
            }
            assert!(frames.iter().any(|f| diff(f, &base) > 1e-3), "{id}: changes the text");
            assert_eq!(frame(&s, 1.6), frames[3], "{id}: deterministic");
        }
    }
}
