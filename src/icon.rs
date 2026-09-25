//! The application icon, drawn at runtime so no image files are needed.

/// RGBA pixels of a `size`×`size` icon: a blue disc with a white paper plane.
pub fn rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    // Paper plane (normalized coordinates).
    let wing = [(0.20, 0.49), (0.78, 0.26), (0.46, 0.60)];
    let body = [(0.46, 0.60), (0.78, 0.26), (0.62, 0.77)];
    let fold = [(0.46, 0.60), (0.54, 0.68), (0.47, 0.75)];
    for y in 0..size {
        for x in 0..size {
            // 4x supersampling for smooth edges.
            let mut acc = [0f32; 4];
            for sy in 0..2 {
                for sx in 0..2 {
                    let px = (x as f32 + 0.25 + 0.5 * sx as f32) / s;
                    let py = (y as f32 + 0.25 + 0.5 * sy as f32) / s;
                    let d = ((px - 0.5).powi(2) + (py - 0.5).powi(2)).sqrt();
                    let c: [f32; 4] = if d > 0.48 {
                        [0.0, 0.0, 0.0, 0.0]
                    } else if in_tri(px, py, &fold) {
                        [200.0, 225.0, 245.0, 255.0]
                    } else if in_tri(px, py, &wing) || in_tri(px, py, &body) {
                        [255.0, 255.0, 255.0, 255.0]
                    } else {
                        // Vertical gradient from light to darker Telegram blue.
                        let t = py;
                        [42.0 - 20.0 * t, 171.0 - 40.0 * t, 238.0 - 30.0 * t, 255.0]
                    };
                    for i in 0..4 {
                        acc[i] += c[i] / 4.0;
                    }
                }
            }
            out.extend(acc.iter().map(|v| v.round() as u8));
        }
    }
    out
}

fn in_tri(px: f32, py: f32, t: &[(f32, f32); 3]) -> bool {
    let sign = |a: (f32, f32), b: (f32, f32)| (px - b.0) * (a.1 - b.1) - (a.0 - b.0) * (py - b.1);
    let d1 = sign(t[0], t[1]);
    let d2 = sign(t[1], t[2]);
    let d3 = sign(t[2], t[0]);
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

pub fn window_icon() -> egui::IconData {
    egui::IconData {
        rgba: rgba(128),
        width: 128,
        height: 128,
    }
}

pub fn tray_icon() -> tray_icon::Icon {
    tray_icon::Icon::from_rgba(rgba(64), 64, 64).expect("valid icon")
}
