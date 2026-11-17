use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Stroke, Vec2};

const CELL: f32 = 13.0;
const GLYPHS: &[char] = &['$', '%', 'X', 't', 'o', '#', '+', 'S', '&', '*', ':', '.', '-'];

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 760.0])
            .with_title("Local Inference"),
        ..Default::default()
    };
    eframe::run_native("local-inference-ui", options, Box::new(|_| Ok(Box::new(App::default()))))
}

struct Message {
    from_user: bool,
    text: String,
}

#[derive(Default)]
struct App {
    input: String,
    messages: Vec<Message>,
}

/// Integer hash -> [0, 1).
fn hash2(x: i32, y: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(374_761_393) ^ (y as u32).wrapping_mul(668_265_263);
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    (h ^ (h >> 16)) as f32 / u32::MAX as f32
}

/// Width (in px) of the centered chat column, kept free of decorations.
const CHAT_WIDTH: f32 = 500.0;

fn draw_background(painter: &egui::Painter, rect: Rect, t: f32) {
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

    // central dark halo (radial gradient via rings) to keep the UI readable
    let center = rect.center();
    let max_r = rect.width().min(rect.height()) * 0.42;
    for i in 0..40 {
        let k = i as f32 / 40.0;
        let r = max_r * (1.0 - k);
        let alpha = (k * k * 22.0) as u8;
        painter.circle_filled(center, r, Color32::from_rgba_unmultiplied(11, 11, 13, alpha));
    }

    draw_pyramids(painter, rect, t);
    draw_bird(painter, rect, t);
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
    let step = (t * 1.5) as i32;

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
                painter.rect_filled(cell, 1.0, fill);
                if n > 0.45 {
                    let g = GLYPHS[(n * 97.0) as usize % GLYPHS.len()];
                    let fg = if lit { Color32::from_gray(25) } else { Color32::from_gray(130) };
                    painter.text(cell.center(), Align2::CENTER_CENTER, g, font.clone(), fg);
                }
            }
        }
    }
}

// Egyptian falcon sprite (flying right), two wing frames.
// '#' body, 'b' lapis wing feathers, 'g' gold band, '@' eye, '>' beak.
const BIRD_UP: &[&str] = &[
    "...bbbb..................",
    "....bbbbb................",
    ".....bggbbb..............",
    "......bgg###.............",
    ".......bgg###......##....",
    "........bgg###....####...",
    ".........#######..##@#>..",
    "##........##########.....",
    ".####################....",
    "...######................",
    "....###..................",
];
const BIRD_DOWN: &[&str] = &[
    ".........................",
    ".........................",
    ".........................",
    ".........................",
    "...................##....",
    "..................####...",
    "..................##@#>..",
    "##........##########.....",
    ".####################....",
    "...######..#ggg###.......",
    "....###.....#ggg##b......",
    ".............##gg#bb.....",
    "..............###bbbb....",
    "................bbbb.....",
];

/// Every `PERIOD` seconds the falcon glides across the screen for `FLIGHT` seconds.
fn draw_bird(painter: &egui::Painter, rect: Rect, t: f32) {
    const PERIOD: f32 = 28.0;
    const FLIGHT: f32 = 11.0;
    let cycle = (t / PERIOD).floor() as i32;
    let local = t - cycle as f32 * PERIOD - 4.0; // short pause after launch
    if !(0.0..FLIGHT).contains(&local) {
        return;
    }
    let p = local / FLIGHT;

    let cols = rect.width() / CELL;
    let rows = rect.height() / CELL;
    let w = BIRD_UP[0].len() as f32;
    // vary the altitude per pass, with a gentle glide wave
    let base = rows * (0.18 + 0.6 * hash2(cycle, 99));
    let ox = (-w + (cols + w * 2.0) * p).floor() as i32;
    let oy = (base + (p * std::f32::consts::TAU * 1.5).sin() * 3.0) as i32;

    // flap a few times, then glide with wings up
    let flapping = (local % 3.0) < 1.6;
    let sprite = if flapping && ((local * 4.0) as i32) % 2 == 1 { BIRD_DOWN } else { BIRD_UP };

    let font = FontId::monospace(CELL * 0.75);
    let step = (t * 6.0) as i32;
    for (ry, line) in sprite.iter().enumerate() {
        for (rx, ch) in line.chars().enumerate() {
            if ch == '.' {
                continue;
            }
            let (gx, gy) = (ox + rx as i32, oy + ry as i32 - 7);
            let cell = Rect::from_min_size(
                rect.min + Vec2::new(gx as f32 * CELL + 1.0, gy as f32 * CELL + 1.0),
                Vec2::splat(CELL - 2.0),
            );
            if !rect.intersects(cell) {
                continue;
            }
            let n = hash2(gx * 13 + step, gy * 7);
            let (fill, glyph) = match ch {
                'b' => (Color32::from_rgb(30 + (n * 25.0) as u8, 70, 150), None),
                'g' => (Color32::from_rgb(205, 160 + (n * 30.0) as u8, 60), None),
                '@' => (Color32::from_rgb(225, 185, 70), Some('o')),
                '>' => (Color32::from_rgb(200, 150, 55), None),
                _ => (Color32::from_gray(150 + (n * 70.0) as u8), Some(GLYPHS[(n * GLYPHS.len() as f32) as usize % GLYPHS.len()])),
            };
            painter.rect_filled(cell, 1.0, fill);
            if let Some(g) = glyph {
                painter.text(cell.center(), Align2::CENTER_CENTER, g, font.clone(), Color32::from_gray(15));
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.set_visuals(egui::Visuals::dark());
        let t = ctx.input(|i| i.time) as f32;
        ctx.request_repaint();

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let rect = ui.max_rect();
                draw_background(ui.painter(), rect, t);

                let panel_w = CHAT_WIDTH.min(rect.width() - 40.0);
                let mut send = false;

                if self.messages.is_empty() {
                    ui.painter().text(
                        rect.center() - Vec2::new(0.0, 70.0),
                        Align2::CENTER_CENTER,
                        "what's on your mind?",
                        FontId::proportional(40.0),
                        Color32::from_gray(235),
                    );
                } else {
                    // message history
                    let hist = Rect::from_center_size(
                        Pos2::new(rect.center().x, rect.top() + (rect.height() - 140.0) / 2.0 + 20.0),
                        Vec2::new(panel_w, rect.height() - 180.0),
                    );
                    ui.scope_builder(egui::UiBuilder::new().max_rect(hist), |ui| {
                        egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                            for m in &self.messages {
                                let layout = if m.from_user {
                                    egui::Layout::right_to_left(egui::Align::TOP)
                                } else {
                                    egui::Layout::left_to_right(egui::Align::TOP)
                                };
                                ui.with_layout(layout, |ui| {
                                    egui::Frame::new()
                                        .fill(if m.from_user {
                                            Color32::from_rgba_unmultiplied(232, 232, 230, 235)
                                        } else {
                                            Color32::from_rgba_unmultiplied(28, 28, 32, 225)
                                        })
                                        .corner_radius(10.0)
                                        .inner_margin(10.0)
                                        .show(ui, |ui| {
                                            ui.set_max_width(panel_w * 0.75);
                                            let c = if m.from_user { Color32::from_gray(15) } else { Color32::from_gray(225) };
                                            ui.label(egui::RichText::new(&m.text).color(c).size(15.0));
                                        });
                                });
                                ui.add_space(8.0);
                            }
                        });
                    });
                }

                // input box
                let input_center = if self.messages.is_empty() {
                    rect.center() + Vec2::new(0.0, 10.0)
                } else {
                    Pos2::new(rect.center().x, rect.bottom() - 70.0)
                };
                let input_rect = Rect::from_center_size(input_center, Vec2::new(panel_w, 76.0));
                ui.scope_builder(egui::UiBuilder::new().max_rect(input_rect), |ui| {
                    egui::Frame::new()
                        .fill(Color32::from_rgba_unmultiplied(22, 22, 26, 235))
                        .stroke(Stroke::new(1.0, Color32::from_gray(50)))
                        .corner_radius(12.0)
                        .inner_margin(10.0)
                        .show(ui, |ui| {
                            ui.set_width(panel_w - 20.0);
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.input)
                                    .hint_text("Ask anything…")
                                    .frame(egui::Frame::NONE)
                                    .desired_width(f32::INFINITY)
                                    .text_color(Color32::from_gray(230))
                                    .font(FontId::proportional(15.0)),
                            );
                            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                send = true;
                                resp.request_focus();
                            }
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("+").color(Color32::from_gray(150)).size(16.0));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.button("↗").clicked() {
                                        send = true;
                                    }
                                    ui.label(egui::RichText::new("↵ send").color(Color32::from_gray(110)).size(11.0));
                                });
                            });
                        });
                });

                if send && !self.input.trim().is_empty() {
                    let text = std::mem::take(&mut self.input);
                    self.messages.push(Message { from_user: true, text: text.clone() });
                    // placeholder: will be replaced by the inference engine (Ollama, etc.)
                    self.messages.push(Message {
                        from_user: false,
                        text: format!("(echo) {text}"),
                    });
                }
            });
    }
}
