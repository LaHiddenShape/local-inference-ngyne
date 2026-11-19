use std::time::Duration;

use eframe::egui::{self, Align2, Color32, FontId, Rect, Stroke, Vec2};

const CELL: f32 = 13.0;
/// Pyramid glyphs flicker this many times per second.
const FLICKER_RATE: f32 = 1.5;
const GLYPHS: &[char] = &['$', '%', 'X', 't', 'o', '#', '+', 'S', '&', '*', ':', '.', '-'];

/// Integer hash -> [0, 1).
fn hash2(x: i32, y: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(374_761_393) ^ (y as u32).wrapping_mul(668_265_263);
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    (h ^ (h >> 16)) as f32 / u32::MAX as f32
}

/// Width (in px) of the centered chat column, kept free of decorations.
pub const CHAT_WIDTH: f32 = 640.0;

/// `animated: false` draws a still scene.
pub fn draw_background(painter: &egui::Painter, rect: Rect, t: f32, animated: bool) {
    painter.rect_filled(rect, 0.0, Color32::from_rgb(11, 11, 13));

    // thin grid
    let grid = Stroke::new(1.0, Color32::from_rgb(24, 24, 28));
    let mut x = rect.left();
    while x < rect.right() {
        painter.vline(x, rect.y_range(), grid);
        x += CELL;
    }
    let mut y = rect.top();
    while y < rect.bottom() {
        painter.hline(rect.x_range(), y, grid);
        y += CELL;
    }

    draw_halo(painter, rect);
    draw_pyramids(painter, rect, if animated { t } else { 0.0 });
}

/// Time until the animated background next changes: the next glyph flicker.
pub fn next_change(t: f32) -> Duration {
    let to_flicker = (1.0 - (t * FLICKER_RATE).fract()) / FLICKER_RATE;
    Duration::from_secs_f32(to_flicker.max(0.02))
}

/// Dark radial gradient behind the chat that fades the grid where the text
/// is, drawn as a single mesh.
fn draw_halo(painter: &egui::Painter, rect: Rect) {
    const RINGS: usize = 16;
    const SEGMENTS: usize = 64;
    let center = rect.center();
    let max_r = rect.width().min(rect.height()) * 0.42;
    // opacity at a fraction `x` of the radius (0 = center)
    let color = |x: f32| {
        let a = 1.0 - (-1.15 * (1.0 - x).powi(3)).exp();
        Color32::from_rgba_unmultiplied(11, 11, 13, (a * 255.0) as u8)
    };
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, color(0.0));
    for ring in 1..=RINGS {
        let x = ring as f32 / RINGS as f32;
        for s in 0..SEGMENTS {
            let angle = s as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            mesh.colored_vertex(center + max_r * x * Vec2::angled(angle), color(x));
        }
    }
    let at = |ring: usize, s: usize| (1 + (ring - 1) * SEGMENTS + s % SEGMENTS) as u32;
    for s in 0..SEGMENTS {
        mesh.add_triangle(0, at(1, s), at(1, s + 1));
        for ring in 1..RINGS {
            let (a, b, c, d) = (at(ring, s), at(ring, s + 1), at(ring + 1, s), at(ring + 1, s + 1));
            mesh.add_triangle(a, b, d);
            mesh.add_triangle(a, d, c);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Pyramid described in grid cells: apex column and height in rows.
/// `depth` 0 = front, larger = further away (dimmer).
struct Pyramid {
    apex: i32,
    height: i32,
    depth: f32,
}

/// Places 2-3 pyramids on the bottom edge, on both sides of the chat column
/// so they never sit behind the input box.
fn layout_pyramids(cols: i32, rows: i32) -> Vec<Pyramid> {
    let half_gap = ((CHAT_WIDTH / 2.0) / CELL).ceil() as i32 + 2;
    let left_zone = cols / 2 - half_gap; // usable cells left of the chat column
    let right_start = cols / 2 + half_gap;
    let right_zone = cols - right_start;
    let max_h = (rows as f32 * 0.32) as i32;

    let mut out = Vec::new();
    if left_zone >= 8 {
        // Giza-like pair: a large pyramid with a smaller one behind it
        let big = (left_zone / 2).min(max_h).max(4);
        let apex = left_zone - big;
        let small = (big * 2 / 3).max(3);
        out.push(Pyramid { apex: apex - big * 3 / 4, height: small, depth: 1.0 });
        out.push(Pyramid { apex, height: big, depth: 0.0 });
    }
    if right_zone >= 6 {
        let h = (right_zone * 2 / 5).min(max_h * 3 / 4).max(3);
        out.push(Pyramid { apex: right_start + right_zone / 2, height: h, depth: 0.5 });
    }
    // draw far ones first
    out.sort_by(|a, b| b.depth.total_cmp(&a.depth));
    out
}

fn draw_pyramids(painter: &egui::Painter, rect: Rect, t: f32) {
    let cols = (rect.width() / CELL) as i32;
    let rows = (rect.height() / CELL) as i32;
    let font = FontId::monospace(CELL * 0.75);
    let step = (t * FLICKER_RATE) as i32;

    for p in layout_pyramids(cols, rows) {
        let dim = 1.0 - p.depth * 0.35;
        for r in 0..p.height {
            let gy = rows - p.height + r;
            for gx in (p.apex - r)..=(p.apex + r) {
                // sun from the left: lit west face, shadowed east face
                let lit = gx < p.apex || (gx == p.apex && r == 0);
                let n = hash2(gx * 31 + (step + gx * 3 + gy) / 4, gy * 17);
                let (base_r, base_g, base_b) = if lit { (150.0, 122.0, 82.0) } else { (78.0, 62.0, 44.0) };
                let v = (0.85 + n * 0.25) * dim;
                let mut fill = Color32::from_rgb(
                    (base_r * v) as u8,
                    (base_g * v) as u8,
                    (base_b * v) as u8,
                );
                if r == 0 {
                    fill = Color32::from_rgb((205.0 * dim) as u8, (165.0 * dim) as u8, (70.0 * dim) as u8); // gilded capstone
                }
                let cell = Rect::from_min_size(
                    rect.min + Vec2::new(gx as f32 * CELL + 1.0, gy as f32 * CELL + 1.0),
                    Vec2::splat(CELL - 2.0),
                );
                painter.rect_filled(cell, 0.0, fill);
                if n > 0.45 {
                    let g = GLYPHS[(n * 97.0) as usize % GLYPHS.len()];
                    let fg = if lit { Color32::from_gray(25) } else { Color32::from_gray(130) };
                    painter.text(cell.center(), Align2::CENTER_CENTER, g, font.clone(), fg);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repaints_at_the_next_flicker() {
        let idle = next_change(20.0).as_secs_f32();
        assert!((idle - 1.0 / FLICKER_RATE).abs() < 1e-3, "{idle}");
    }
}
