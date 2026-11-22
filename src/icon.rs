//! The app icon, drawn in code: a dark rounded tile with syntax-coloured lines of code and
//! three carets in a column (multi-caret editing). Used for the window icon at runtime and
//! by `examples/mkicon.rs` to generate the .ico / .icns / .png files in `assets/`.

/// Colours from the editor's default theme (base16 ocean).
const BG_TOP: [f32; 3] = [0x3a as f32, 0x42 as f32, 0x55 as f32];
const BG_BOTTOM: [f32; 3] = [0x1b as f32, 0x1f as f32, 0x27 as f32];
const PURPLE: [f32; 3] = [0xb4 as f32, 0x8e as f32, 0xad as f32];
const BLUE: [f32; 3] = [0x8f as f32, 0xa1 as f32, 0xb3 as f32];
const GREEN: [f32; 3] = [0xa3 as f32, 0xbe as f32, 0x8c as f32];
const YELLOW: [f32; 3] = [0xeb as f32, 0xcb as f32, 0x8b as f32];
const ORANGE: [f32; 3] = [0xd0 as f32, 0x87 as f32, 0x70 as f32];
const CARET: [f32; 3] = [0xff as f32, 0xff as f32, 0xff as f32];

/// Tile bounds (unit square, with a margin like macOS icons) and corner radius.
const TILE: (f32, f32) = (0.07, 0.93);
const RADIUS: f32 = 0.2;
const BAR: f32 = 0.034; // half thickness of a code line

/// (x0, x1, y, colour) of each code token.
const TOKENS: &[(f32, f32, f32, [f32; 3])] = &[
    (0.20, 0.38, 0.27, PURPLE),
    (0.48, 0.72, 0.27, BLUE),
    (0.30, 0.56, 0.39, GREEN),
    (0.30, 0.38, 0.51, YELLOW),
    (0.48, 0.56, 0.51, ORANGE),
    (0.30, 0.56, 0.63, BLUE),
    (0.20, 0.34, 0.75, PURPLE),
];
/// Carets: x and the lines they sit on.
const CARET_X: f32 = 0.645;
const CARET_LINES: &[f32] = &[0.39, 0.51, 0.63];

fn rounded_box(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (hx, hy) = ((x1 - x0) / 2.0 - r, (y1 - y0) / 2.0 - r);
    let (dx, dy) = ((px - cx).abs() - hx, (py - cy).abs() - hy);
    let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
    outside + dx.max(dy).min(0.0) - r
}

/// Signed distance to a horizontal capsule from x0 to x1 at height y.
fn capsule(px: f32, py: f32, x0: f32, x1: f32, y: f32, half: f32) -> f32 {
    let cx = px.clamp(x0, x1);
    ((px - cx).powi(2) + (py - y).powi(2)).sqrt() - half
}

/// Colour (premultiplied rgb, alpha) of the point (x, y) in the unit square.
fn sample(x: f32, y: f32) -> ([f32; 3], f32) {
    if rounded_box(x, y, TILE.0, TILE.0, TILE.1, TILE.1, RADIUS) > 0.0 {
        return ([0.0; 3], 0.0);
    }
    let t = ((y - TILE.0) / (TILE.1 - TILE.0)).clamp(0.0, 1.0);
    let mut c = [0.0; 3];
    for i in 0..3 {
        c[i] = BG_TOP[i] + (BG_BOTTOM[i] - BG_TOP[i]) * t;
    }
    for &(x0, x1, ty, col) in TOKENS {
        if capsule(x, y, x0, x1, ty, BAR) <= 0.0 {
            c = col;
        }
    }
    for &ly in CARET_LINES {
        if rounded_box(x, y, CARET_X - 0.016, ly - 0.05, CARET_X + 0.016, ly + 0.05, 0.008) <= 0.0 {
            c = CARET;
        }
    }
    (c, 1.0)
}

/// Straight (non-premultiplied) RGBA pixels of a `size` x `size` icon.
pub fn rgba(size: u32) -> Vec<u8> {
    let ss = if size <= 64 { 8 } else { 4 }; // supersampling per axis
    let n = (ss * ss) as f32;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (mut acc, mut a) = ([0.0f32; 3], 0.0f32);
            for sy in 0..ss {
                for sx in 0..ss {
                    let x = (px as f32 + (sx as f32 + 0.5) / ss as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / ss as f32) / size as f32;
                    let (c, ca) = sample(x, y);
                    for i in 0..3 {
                        acc[i] += c[i] * ca;
                    }
                    a += ca;
                }
            }
            if a == 0.0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                for v in acc {
                    out.push((v / a).round().clamp(0.0, 255.0) as u8);
                }
                out.push((a / n * 255.0).round() as u8);
            }
        }
    }
    out
}
