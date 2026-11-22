//! IntelliJ-style side-by-side diff: read-only left pane, editable right pane (the real
//! document), curved bands linking the changes, "»" to revert a change, synced scrolling.

use crate::diff::{self, line_pos, Hunk, Kind};
use crate::doc::{Document, Edit, EditKind};
use crate::editor::{self, Decor, Settings, View};
use crate::highlight::{Colors, Syntax};
use crate::selection::Sel;
use eframe::egui::{
    self, pos2, vec2, Align2, Color32, FontId, Id, Pos2, Rect, Sense, Shape, Stroke, UiBuilder,
};
use syntect::highlighting::Theme;

pub struct DiffState {
    pub left: Document,
    pub left_view: View,
    pub left_title: String,
    pub right_title: String,
    pub hunks: Vec<Hunk>,
    key: (u64, u64),
    goto_first: bool,
}

impl DiffState {
    pub fn new(left_text: &str, left_title: String, right: &Document, right_view: &View) -> Self {
        let mut left = Document::from_str(left_text);
        left.tab_size = right.tab_size;
        left.camel_humps = right.camel_humps;
        DiffState {
            left,
            left_view: View::new(right_view.cache.syntax.clone()),
            left_title,
            right_title: format!("{} (editable)", right.name()),
            hunks: vec![],
            key: (u64::MAX, u64::MAX),
            goto_first: true,
        }
    }

    fn refresh(&mut self, right: &Document) {
        let key = (self.left.version, right.version);
        if key != self.key {
            self.hunks = diff::compute(&self.left.rope, &right.rope);
            self.key = key;
        }
    }

    /// F7 / Shift+F7: move to the next/previous difference on the focused side.
    pub fn navigate(&mut self, right: &mut Document, right_view: &mut View, forward: bool, left_focused: bool) {
        self.refresh(right);
        let doc = if left_focused { &self.left } else { &*right };
        let line = doc.rope.char_to_line(doc.primary_sel().head);
        let Some(i) = diff::neighbour(&self.hunks, line, !left_focused, forward) else { return };
        self.select_hunk(i, right, right_view);
    }

    fn select_hunk(&mut self, i: usize, right: &mut Document, right_view: &mut View) {
        let h = &self.hunks[i];
        let rp = line_pos(&right.rope, h.right.start);
        right.set_sels(vec![Sel::caret(rp)], 0);
        right_view.center_caret = true;
        let lp = line_pos(&self.left.rope, h.left.start);
        self.left.set_sels(vec![Sel::caret(lp)], 0);
    }
}

/// Replace the right side of a change with the left side (undoable).
fn revert(doc: &mut Document, left: &ropey::Rope, h: &Hunk) {
    let text = left.slice(line_pos(left, h.left.start)..line_pos(left, h.left.end)).to_string();
    let (rs, re) = (line_pos(&doc.rope, h.right.start), line_pos(&doc.rope, h.right.end));
    doc.set_sels(vec![Sel::caret(rs)], 0);
    doc.apply(EditKind::Other, vec![Edit { start: rs, end: re, text, anchor: 0, head: 0 }]);
}

// IntelliJ-like palette (translucent so it works on any theme)
const ADDED: Color32 = Color32::from_rgba_premultiplied(30, 80, 45, 70);
const ADDED_WORD: Color32 = Color32::from_rgba_premultiplied(45, 120, 60, 110);
const DELETED: Color32 = Color32::from_rgba_premultiplied(60, 60, 60, 70);
const DELETED_WORD: Color32 = Color32::from_rgba_premultiplied(95, 95, 95, 110);
const MODIFIED: Color32 = Color32::from_rgba_premultiplied(30, 60, 100, 70);
const MODIFIED_WORD: Color32 = Color32::from_rgba_premultiplied(45, 95, 160, 120);

fn colors(kind: Kind) -> (Color32, Color32) {
    match kind {
        Kind::Insert => (ADDED, ADDED_WORD),
        Kind::Delete => (DELETED, DELETED_WORD),
        Kind::Replace => (MODIFIED, MODIFIED_WORD),
    }
}

fn decorations(hunks: &[Hunk]) -> (Decor, Decor) {
    let (mut l, mut r) = (Decor::default(), Decor::default());
    for h in hunks {
        let (bg, strong) = colors(h.kind);
        match h.kind {
            Kind::Insert => {
                r.lines.push((h.right.start, h.right.end, bg));
                l.marks.push((h.left.start, strong));
            }
            Kind::Delete => {
                l.lines.push((h.left.start, h.left.end, bg));
                r.marks.push((h.right.start, strong));
            }
            Kind::Replace => {
                l.lines.push((h.left.start, h.left.end, bg));
                r.lines.push((h.right.start, h.right.end, bg));
                l.inline.extend(h.left_words.iter().map(|w| (w.start, w.end, strong)));
                r.inline.extend(h.right_words.iter().map(|w| (w.start, w.end, strong)));
            }
        }
    }
    (l, r)
}

pub struct Ctx<'a> {
    pub syn: &'a Syntax,
    pub theme: &'a Theme,
    pub colors: &'a Colors,
    pub settings: &'a Settings,
    pub find: &'a [(usize, usize)],
    pub right_id: Id,
    pub left_id: Id,
}

pub enum Outcome {
    None,
    Close,
}

const DIVIDER_W: f32 = 46.0;
const TITLE_H: f32 = 24.0;

pub fn show(ui: &mut egui::Ui, right: &mut Document, view: &mut View, st: &mut DiffState, cx: &Ctx) -> Outcome {
    st.refresh(right);
    if st.goto_first && !st.hunks.is_empty() {
        st.goto_first = false;
        st.select_hunk(0, right, view);
    }
    let left_focused = ui.memory(|m| m.has_focus(cx.left_id));
    let mut outcome = Outcome::None;

    // ---- toolbar
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        if ui.button("▲").on_hover_text("Previous difference (Shift+F7)").clicked() {
            st.navigate(right, view, false, left_focused);
        }
        if ui.button("▼").on_hover_text("Next difference (F7)").clicked() {
            st.navigate(right, view, true, left_focused);
        }
        let n = st.hunks.len();
        ui.label(match n {
            0 => "Contents are identical".to_string(),
            1 => "1 difference".to_string(),
            n => format!("{n} differences"),
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("× Close diff").on_hover_text("Esc").clicked() {
                outcome = Outcome::Close;
            }
            if n > 0
                && ui
                    .button("« Revert All")
                    .on_hover_text("Replace the whole right side with the left side (Ctrl+Z undoes)")
                    .clicked()
            {
                let head = right.primary_sel().head;
                let text = st.left.rope.to_string();
                let end = right.rope.len_chars();
                right.apply(EditKind::Other, vec![Edit { start: 0, end, text, anchor: 0, head: 0 }]);
                let c = head.min(right.rope.len_chars());
                right.set_sels(vec![Sel::caret(c)], 0);
            }
        });
    });

    // ---- layout
    let full = ui.available_rect_before_wrap();
    let half = ((full.width() - DIVIDER_W) / 2.0).max(50.0);
    let titles = Rect::from_min_size(full.min, vec2(full.width(), TITLE_H));
    let body = Rect::from_min_max(pos2(full.left(), titles.bottom()), full.max);
    let lrect = Rect::from_min_size(body.min, vec2(half, body.height()));
    let drect = Rect::from_min_size(pos2(lrect.right(), body.top()), vec2(DIVIDER_W, body.height()));
    let rrect = Rect::from_min_max(pos2(drect.right(), body.top()), body.max);

    let painter = ui.painter_at(full);
    painter.rect_filled(titles, 0.0, cx.colors.panel);
    let title_font = FontId::proportional(13.0);
    let tcol = cx.colors.gutter_fg_active;
    painter.text(pos2(lrect.left() + 10.0, titles.center().y), Align2::LEFT_CENTER, &st.left_title, title_font.clone(), tcol);
    painter.text(pos2(rrect.left() + 10.0, titles.center().y), Align2::LEFT_CENTER, &st.right_title, title_font, tcol);

    // Which pane drives scrolling this frame: the one under the pointer when it scrolls or
    // drags, otherwise the focused one.
    let (hover, wheel, down) =
        ui.input(|i| (i.pointer.hover_pos(), i.smooth_scroll_delta != egui::Vec2::ZERO, i.pointer.any_down()));
    let left_drives = match hover {
        Some(p) if (wheel || down) && lrect.contains(p) => true,
        Some(p) if (wheel || down) && rrect.contains(p) => false,
        _ => left_focused,
    };

    // ---- panes
    let (ldec, rdec) = decorations(&st.hunks);
    let settings = Settings { minimap: false, ..cx.settings.clone() };
    let mk = |id: Id, find, decor| editor::Ctx {
        syn: cx.syn,
        theme: cx.theme,
        colors: cx.colors,
        settings: &settings,
        find,
        id,
        decor: Some(decor),
    };
    ui.allocate_new_ui(UiBuilder::new().max_rect(lrect), |ui| {
        editor::show(ui, &mut st.left, &mut st.left_view, &mk(cx.left_id, &[], &ldec));
    });
    ui.allocate_new_ui(UiBuilder::new().max_rect(rrect), |ui| {
        editor::show(ui, right, view, &mk(cx.right_id, cx.find, &rdec));
    });

    // ---- divider with bands linking the changes
    let lh = view.line_height.max(1.0);
    let ly = |l: usize| st.left_view.text_rect.top() + l as f32 * lh - st.left_view.scroll.y;
    let ry = |l: usize| view.text_rect.top() + l as f32 * lh - view.scroll.y;
    let dp = ui.painter_at(drect);
    dp.rect_filled(drect, 0.0, cx.colors.panel);
    let mut revert_idx = None;
    for (i, h) in st.hunks.iter().enumerate() {
        let (l0, l1, r0, r1) = (ly(h.left.start), ly(h.left.end), ry(h.right.start), ry(h.right.end));
        if (l1 < drect.top() && r1 < drect.top()) || (l0 > drect.bottom() && r0 > drect.bottom()) {
            continue;
        }
        let (bg, strong) = colors(h.kind);
        const N: usize = 16;
        let pts: Vec<(f32, f32, f32)> = (0..=N)
            .map(|k| {
                let t = k as f32 / N as f32;
                let s = t * t * (3.0 - 2.0 * t); // smoothstep: S-shaped like IntelliJ
                (drect.left() + t * drect.width(), l0 + (r0 - l0) * s, l1 + (r1 - l1) * s)
            })
            .collect();
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            dp.add(Shape::convex_polygon(
                vec![pos2(a.0, a.1), pos2(b.0, b.1), pos2(b.0, b.2), pos2(a.0, a.2)],
                bg,
                Stroke::NONE,
            ));
        }
        let edge = |f: fn(&(f32, f32, f32)) -> f32| pts.iter().map(|p| Pos2::new(p.0, f(p))).collect::<Vec<_>>();
        let stroke = Stroke::new(1.0_f32, strong.to_opaque().gamma_multiply(0.8));
        dp.add(Shape::line(edge(|p| p.1), stroke));
        dp.add(Shape::line(edge(|p| p.2), stroke));

        // "»" revert button at the top-left of the band
        let br = Rect::from_min_size(pos2(drect.left() + 2.0, l0.max(drect.top()) + 1.0), vec2(18.0, 16.0));
        if drect.contains(br.center()) {
            let resp = ui
                .interact(br, cx.left_id.with(("revert", i)), Sense::click())
                .on_hover_text("Revert: replace the right side with the left side");
            let col = if resp.hovered() { cx.colors.fg } else { cx.colors.gutter_fg_active };
            dp.text(br.center(), Align2::CENTER_CENTER, "»", FontId::monospace(15.0), col);
            if resp.clicked() {
                revert_idx = Some(i);
            }
        }
    }
    if let Some(i) = revert_idx {
        let h = st.hunks[i].clone();
        revert(right, &st.left.rope, &h);
        view.scroll_to_caret = true;
    }

    // ---- keep both panes aligned on corresponding lines
    // (clamped to the follower's own range, or the editor would clamp it back every frame)
    let (before_l, before_r) = (st.left_view.scroll, view.scroll);
    let max_y = |d: &Document| (d.rope.len_lines().saturating_sub(1)) as f32 * lh;
    if left_drives {
        let y = diff::map_line(&st.hunks, st.left_view.scroll.y / lh, true) * lh;
        view.scroll.y = y.clamp(0.0, max_y(right));
        view.scroll.x = st.left_view.scroll.x;
    } else {
        let y = diff::map_line(&st.hunks, view.scroll.y / lh, false) * lh;
        st.left_view.scroll.y = y.clamp(0.0, max_y(&st.left));
        st.left_view.scroll.x = view.scroll.x;
    }
    if (st.left_view.scroll - before_l).length() > 0.5 || (view.scroll - before_r).length() > 0.5 {
        ui.ctx().request_repaint();
    }
    outcome
}
