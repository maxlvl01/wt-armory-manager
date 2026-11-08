//! Small vector icons painted with egui shapes, so they scale and take any colour.

use crate::api::Kind;
use eframe::egui::{self, pos2, vec2, Color32, Painter, Pos2, Rect, Shape, Stroke};

#[derive(Clone, Copy)]
pub enum Icon {
    Kind(Kind),
    Heart,
    Download,
    Folder,
    Gear,
    Library,
    Check,
    Refresh,
    Search,
    External,
    Close,
    Bug,
    Star(bool),
    User,
    Comment,
    Login,
    Update,
}

pub fn show(ui: &mut egui::Ui, icon: Icon, size: f32, color: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), egui::Sense::hover());
    paint(ui.painter(), rect, icon, color);
    resp
}

fn arc(c: Pos2, r: f32, a0: f32, a1: f32, n: usize) -> Vec<Pos2> {
    (0..=n)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / n as f32;
            pos2(c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect()
}

pub fn paint(p: &Painter, r: Rect, icon: Icon, c: Color32) {
    let s = r.width();
    let at = |x: f32, y: f32| pos2(r.left() + x * s, r.top() + y * s);
    let st = Stroke::new((s * 0.09).max(1.3), c);
    let box_ = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(at(x0, y0), at(x1, y1));
    use std::f32::consts::{FRAC_PI_2, PI};
    match icon {
        Icon::Kind(Kind::Camouflage) => {
            // patchwork of filled and outlined squares
            p.rect_filled(box_(0.1, 0.1, 0.47, 0.47), 1.5, c);
            p.rect_filled(box_(0.53, 0.53, 0.9, 0.9), 1.5, c);
            p.rect_stroke(box_(0.55, 0.12, 0.88, 0.45), 1.5, st);
            p.rect_stroke(box_(0.12, 0.55, 0.45, 0.88), 1.5, st);
        }
        Icon::Kind(Kind::Sound) => {
            p.add(Shape::convex_polygon(
                vec![at(0.08, 0.38), at(0.28, 0.38), at(0.52, 0.16), at(0.52, 0.84), at(0.28, 0.62), at(0.08, 0.62)],
                c,
                Stroke::NONE,
            ));
            p.add(Shape::line(arc(at(0.52, 0.5), 0.18 * s, -0.8, 0.8, 10), st));
            p.add(Shape::line(arc(at(0.52, 0.5), 0.36 * s, -0.85, 0.85, 14), st));
        }
        Icon::Kind(Kind::Sight) => {
            let ctr = at(0.5, 0.5);
            p.circle_stroke(ctr, 0.32 * s, st);
            for (a, b) in [((0.5, 0.04), (0.5, 0.32)), ((0.5, 0.68), (0.5, 0.96)), ((0.04, 0.5), (0.32, 0.5)), ((0.68, 0.5), (0.96, 0.5))] {
                p.line_segment([at(a.0, a.1), at(b.0, b.1)], st);
            }
            p.circle_filled(ctr, 0.06 * s, c);
        }
        Icon::Kind(Kind::Mission) => {
            p.line_segment([at(0.22, 0.08), at(0.22, 0.94)], st);
            p.add(Shape::convex_polygon(vec![at(0.22, 0.1), at(0.86, 0.3), at(0.22, 0.52)], c, Stroke::NONE));
        }
        Icon::Kind(Kind::Model) => {
            // isometric cube
            let top = at(0.5, 0.08);
            let (l, rr) = (at(0.1, 0.3), at(0.9, 0.3));
            let mid = at(0.5, 0.52);
            let (bl, br, bot) = (at(0.1, 0.74), at(0.9, 0.74), at(0.5, 0.96));
            p.add(Shape::closed_line(vec![top, rr, br, bot, bl, l], st));
            p.add(Shape::line(vec![l, mid, rr], st));
            p.line_segment([mid, bot], st);
        }
        Icon::Kind(Kind::Location) => {
            // hangar: arched roof with a door
            let mut roof = arc(at(0.5, 0.82), 0.4 * s, PI, 2.0 * PI, 16);
            roof.push(at(0.9, 0.86));
            roof.insert(0, at(0.1, 0.86));
            p.add(Shape::line(roof, st));
            p.line_segment([at(0.04, 0.88), at(0.96, 0.88)], st);
            p.rect_filled(box_(0.36, 0.56, 0.64, 0.86), 1.0, c);
        }
        Icon::Kind(Kind::Controls) => {
            p.rect_stroke(box_(0.05, 0.28, 0.95, 0.76), 0.2 * s, st);
            p.line_segment([at(0.2, 0.52), at(0.4, 0.52)], st);
            p.line_segment([at(0.3, 0.42), at(0.3, 0.62)], st);
            p.circle_filled(at(0.65, 0.46), 0.055 * s, c);
            p.circle_filled(at(0.77, 0.58), 0.055 * s, c);
        }
        Icon::Heart => {
            p.circle_filled(at(0.31, 0.36), 0.21 * s, c);
            p.circle_filled(at(0.69, 0.36), 0.21 * s, c);
            p.add(Shape::convex_polygon(vec![at(0.12, 0.46), at(0.88, 0.46), at(0.5, 0.88)], c, Stroke::NONE));
        }
        Icon::Download => {
            p.line_segment([at(0.5, 0.08), at(0.5, 0.56)], st);
            p.add(Shape::convex_polygon(vec![at(0.24, 0.46), at(0.76, 0.46), at(0.5, 0.74)], c, Stroke::NONE));
            p.line_segment([at(0.12, 0.9), at(0.88, 0.9)], st);
        }
        Icon::Folder => {
            p.add(Shape::closed_line(
                vec![at(0.06, 0.2), at(0.38, 0.2), at(0.48, 0.32), at(0.94, 0.32), at(0.94, 0.84), at(0.06, 0.84)],
                st,
            ));
        }
        Icon::Gear => {
            let ctr = at(0.5, 0.5);
            for i in 0..8 {
                let a = i as f32 * PI / 4.0;
                let d = vec2(a.cos(), a.sin());
                p.line_segment([ctr + d * 0.28 * s, ctr + d * 0.46 * s], Stroke::new(st.width * 1.6, c));
            }
            p.circle_stroke(ctr, 0.3 * s, st);
            p.circle_stroke(ctr, 0.1 * s, st);
        }
        Icon::Library => {
            p.rect_stroke(box_(0.1, 0.12, 0.3, 0.9), 1.0, st);
            p.rect_stroke(box_(0.4, 0.12, 0.6, 0.9), 1.0, st);
            p.add(Shape::closed_line(vec![at(0.68, 0.2), at(0.86, 0.14), at(0.98, 0.84), at(0.8, 0.9)], st));
        }
        Icon::Check => {
            p.add(Shape::line(vec![at(0.12, 0.52), at(0.4, 0.78), at(0.9, 0.24)], Stroke::new(st.width * 1.3, c)));
        }
        Icon::Refresh => {
            let pts = arc(at(0.5, 0.5), 0.34 * s, -FRAC_PI_2 + 0.5, PI * 1.5 - 0.1, 20);
            let end = *pts.last().unwrap();
            p.add(Shape::line(pts, st));
            p.add(Shape::convex_polygon(
                vec![end + vec2(-0.16, -0.1) * s, end + vec2(0.14, -0.14) * s, end + vec2(0.04, 0.16) * s],
                c,
                Stroke::NONE,
            ));
        }
        Icon::Search => {
            p.circle_stroke(at(0.42, 0.42), 0.28 * s, st);
            p.line_segment([at(0.62, 0.62), at(0.92, 0.92)], Stroke::new(st.width * 1.4, c));
        }
        Icon::External => {
            p.add(Shape::line(vec![at(0.45, 0.15), at(0.12, 0.15), at(0.12, 0.88), at(0.85, 0.88), at(0.85, 0.55)], st));
            p.line_segment([at(0.42, 0.58), at(0.88, 0.12)], st);
            p.add(Shape::line(vec![at(0.58, 0.12), at(0.88, 0.12), at(0.88, 0.42)], st));
        }
        Icon::Close => {
            p.line_segment([at(0.2, 0.2), at(0.8, 0.8)], st);
            p.line_segment([at(0.8, 0.2), at(0.2, 0.8)], st);
        }
        Icon::Bug => {
            // beetle: oval body, head, three legs per side, antennae
            p.add(Shape::ellipse_filled(at(0.5, 0.6), vec2(0.22 * s, 0.3 * s), c));
            p.circle_filled(at(0.5, 0.25), 0.11 * s, c);
            for (y, dx) in [(0.45, 0.42), (0.6, 0.44), (0.76, 0.4)] {
                p.line_segment([at(0.5, y), at(0.5 - dx, y - 0.06)], st);
                p.line_segment([at(0.5, y), at(0.5 + dx, y - 0.06)], st);
            }
            p.line_segment([at(0.44, 0.18), at(0.32, 0.04)], st);
            p.line_segment([at(0.56, 0.18), at(0.68, 0.04)], st);
        }
        Icon::Star(filled) => {
            let ctr = at(0.5, 0.54);
            let pts: Vec<Pos2> = (0..10)
                .map(|i| {
                    let a = -FRAC_PI_2 + i as f32 * PI / 5.0;
                    let rad = if i % 2 == 0 { 0.46 } else { 0.19 } * s;
                    ctr + vec2(a.cos(), a.sin()) * rad
                })
                .collect();
            if filled {
                // a star is not convex: fill it as a fan of triangles around the centre
                for i in 0..10 {
                    p.add(Shape::convex_polygon(vec![ctr, pts[i], pts[(i + 1) % 10]], c, Stroke::NONE));
                }
            } else {
                p.add(Shape::closed_line(pts, Stroke::new(st.width * 0.85, c)));
            }
        }
        Icon::User => {
            p.circle_stroke(at(0.5, 0.32), 0.19 * s, st);
            p.add(Shape::line(arc(at(0.5, 0.98), 0.4 * s, PI + 0.15, 2.0 * PI - 0.15, 16), st));
        }
        Icon::Comment => {
            p.add(Shape::closed_line(
                vec![at(0.1, 0.16), at(0.9, 0.16), at(0.9, 0.7), at(0.46, 0.7), at(0.24, 0.9), at(0.26, 0.7), at(0.1, 0.7)],
                st,
            ));
        }
        Icon::Login => {
            p.add(Shape::line(vec![at(0.5, 0.12), at(0.88, 0.12), at(0.88, 0.88), at(0.5, 0.88)], st));
            p.line_segment([at(0.1, 0.5), at(0.62, 0.5)], st);
            p.add(Shape::line(vec![at(0.46, 0.32), at(0.64, 0.5), at(0.46, 0.68)], st));
        }
        Icon::Update => {
            // arrow up out of a tray
            p.line_segment([at(0.5, 0.66), at(0.5, 0.16)], st);
            p.add(Shape::convex_polygon(vec![at(0.5, 0.04), at(0.76, 0.32), at(0.24, 0.32)], c, Stroke::NONE));
            p.add(Shape::line(vec![at(0.1, 0.62), at(0.1, 0.9), at(0.9, 0.9), at(0.9, 0.62)], st));
        }
    }
}
