//! The editor widget: custom painting of text, carets, selections, gutter, minimap.

use crate::doc::{rect_sels, Block, Document};
use crate::highlight::{Colors, LineCache, Syntax};
use crate::selection::Sel;
use crate::text::*;
use eframe::egui::{
    self, pos2, text::LayoutJob, vec2, Align2, Color32, EventFilter, FontId, Id, PointerButton,
    Pos2, Rect, Response, Sense, Stroke, TextFormat, Ui, Vec2,
};
use std::time::Duration;
use syntect::highlighting::Theme;

#[derive(Clone, Copy)]
enum DragMode {
    Char,
    Word,
    Line,
    /// Rectangular selection clamped to line ends (Alt+drag, middle drag).
    Column { line: usize, vcol: usize },
    /// Column Selection Mode: rectangle with virtual space.
    Block,
}

struct Drag {
    mode: DragMode,
    origin: Sel,
    /// Selections kept while dragging (Ctrl+Alt+Shift+drag adds another rectangle).
    base: Vec<Sel>,
}

impl Drag {
    fn new(mode: DragMode, origin: Sel) -> Self {
        Drag { mode, origin, base: vec![] }
    }
}

pub struct View {
    pub scroll: Vec2,
    pub cache: LineCache,
    drag: Option<Drag>,
    last_click: Option<(f64, usize, u32)>,
    pub scroll_to_caret: bool,
    pub center_caret: bool,
    pub page_lines: usize,
    blink_epoch: f64,
    blink_sig: (u64, usize, usize),
    mm_drag: bool,
    sb_drag: Option<f32>,
    /// Layout from the last frame (used to link the diff panes).
    pub text_rect: Rect,
    pub line_height: f32,
}

impl View {
    pub fn new(syntax: String) -> Self {
        Self {
            scroll: Vec2::ZERO,
            cache: LineCache::new(syntax),
            drag: None,
            last_click: None,
            scroll_to_caret: false,
            center_caret: false,
            page_lines: 30,
            blink_epoch: 0.0,
            blink_sig: (0, 0, 0),
            mm_drag: false,
            sb_drag: None,
            text_rect: Rect::NOTHING,
            line_height: 20.0,
        }
    }
}

#[derive(Clone)]
pub struct Settings {
    pub font_size: f32,
    pub line_spacing: f32,
    pub minimap: bool,
}

/// Extra background decorations (used by the diff view).
#[derive(Default)]
pub struct Decor {
    /// Full-width line backgrounds: (first line, end line exclusive, color).
    pub lines: Vec<(usize, usize, Color32)>,
    /// Char ranges painted on top of the line backgrounds (changed words).
    pub inline: Vec<(usize, usize, Color32)>,
    /// Thin horizontal markers at the top of a line (where the other side inserted lines).
    pub marks: Vec<(usize, Color32)>,
}

pub struct Ctx<'a> {
    pub syn: &'a Syntax,
    pub theme: &'a Theme,
    pub colors: &'a Colors,
    pub settings: &'a Settings,
    pub find: &'a [(usize, usize)],
    pub id: Id,
    pub decor: Option<&'a Decor>,
}

const MINIMAP_W: f32 = 110.0;
const MM_LH: f32 = 3.0;
const MM_CW: f32 = 1.3;
const SB_W: f32 = 12.0;

pub fn show(ui: &mut Ui, doc: &mut Document, view: &mut View, cx: &Ctx) -> Response {
    let rect = ui.available_rect_before_wrap();
    let resp = ui.interact(rect, cx.id, Sense::click_and_drag());
    ui.memory_mut(|m| {
        m.set_focus_lock_filter(
            cx.id,
            EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
        )
    });
    let font = FontId::monospace(cx.settings.font_size);
    let (cw, row_h) = ui.fonts(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let lh = (row_h * cx.settings.line_spacing).round().max(1.0);
    let tab = doc.tab_size;
    let nlines = doc.rope.len_lines();
    let digits = nlines.to_string().len().max(3);
    let gutter_w = digits as f32 * cw + cw * 3.0;
    let mm_w = if cx.settings.minimap { MINIMAP_W } else { 0.0 };
    let gutter = Rect::from_min_size(rect.min, vec2(gutter_w, rect.height()));
    let mm_rect = Rect::from_min_max(pos2(rect.max.x - mm_w, rect.min.y), rect.max);
    let sb_rect = Rect::from_min_max(pos2(mm_rect.min.x - SB_W, rect.min.y), pos2(mm_rect.min.x, rect.max.y));
    let text_rect = Rect::from_min_max(pos2(gutter.max.x, rect.min.y), pos2(sb_rect.min.x, rect.max.y));
    let pad = cw * 0.5;
    view.text_rect = text_rect;
    view.line_height = lh;
    let vis_h = text_rect.height();
    view.page_lines = ((vis_h / lh) as usize).saturating_sub(1).max(1);
    let max_scroll_y = (nlines.saturating_sub(1)) as f32 * lh;

    if let Some(l) = doc.dirty_line.take() {
        view.cache.invalidate(l);
    }

    // ------------------------------------------------------------ input: wheel
    let (pointer, hover, pressed, down, mods, time, wheel) = ui.input(|i| {
        (
            i.pointer.interact_pos(),
            i.pointer.hover_pos(),
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.modifiers,
            i.time,
            i.smooth_scroll_delta,
        )
    });
    if hover.is_some_and(|p| rect.contains(p)) && ui.ctx().layer_id_at(hover.unwrap()).is_some_and(|l| l == ui.layer_id()) {
        view.scroll -= wheel;
    }

    let pos_at = |p: Pos2, doc: &Document, scroll: Vec2| -> (usize, usize, usize) {
        let line = (((p.y - text_rect.top() + scroll.y) / lh).floor().max(0.0) as usize).min(nlines - 1);
        let vcol = ((p.x - text_rect.left() - pad + scroll.x) / cw).round().max(0.0) as usize;
        let col = col_from_visual(&doc.rope, line, vcol, tab);
        (doc.rope.line_to_char(line) + col, line, vcol)
    };

    // ------------------------------------------------------------ input: mouse press
    let over_editor = pointer.is_some_and(|p| rect.contains(p))
        && ui.ctx().layer_id_at(pointer.unwrap()).is_some_and(|l| l == ui.layer_id());
    let mid_pressed = ui.input(|i| i.pointer.button_pressed(PointerButton::Middle));
    if (pressed || mid_pressed) && over_editor {
        let p = pointer.unwrap();
        resp.request_focus();
        let ctrl = mods.ctrl || mods.command;
        if pressed && mm_w > 0.0 && mm_rect.contains(p) {
            view.mm_drag = true;
        } else if pressed && sb_rect.contains(p) {
            let (ty, th) = thumb(sb_rect, view.scroll.y, max_scroll_y, vis_h);
            view.sb_drag = Some(if p.y >= ty && p.y <= ty + th { p.y - ty } else { th / 2.0 });
        } else if mid_pressed {
            let (pos, line, vcol) = pos_at(p, doc, view.scroll);
            if mods.alt && mods.shift {
                // IntelliJ: Alt+Shift+Middle-Click selects the rectangle from the caret to here
                let s = doc.primary_sel();
                let (hl, hc) = pos_to_lc(&doc.rope, s.head);
                let hv = visual_col(&doc.rope, hl, hc, tab);
                let (sels, pr) = rect_sels(&doc.rope, tab, hl, hv, line, vcol);
                doc.set_sels(sels, pr);
                view.drag = None;
            } else {
                // Sublime: middle-button drag selects a rectangle
                doc.set_sels(vec![Sel::caret(pos)], 0);
                view.drag = Some(Drag::new(DragMode::Column { line, vcol }, Sel::caret(pos)));
            }
        } else {
            let (pos, line, vcol) = pos_at(p, doc, view.scroll);
            let count = match view.last_click {
                Some((t, lp, c)) if time - t < 0.4 && lp == pos => c % 3 + 1,
                _ => 1,
            };
            view.last_click = Some((time, pos, count));
            let rope = &doc.rope;
            let add_word_or_caret = |doc: &mut Document| {
                if count == 2 {
                    let r = double_click_range(&doc.rope, pos);
                    let mut sels = doc.sels.clone();
                    sels.retain(|s| !(s.is_empty() && s.head == pos));
                    sels.push(Sel::new(r.start, r.end));
                    let n = sels.len() - 1;
                    doc.set_sels(sels, n);
                } else {
                    doc.add_caret_at(pos);
                }
            };
            if gutter.contains(p) {
                let a = rope.line_to_char(line);
                let b = if line + 1 < nlines { rope.line_to_char(line + 1) } else { rope.len_chars() };
                let s = Sel::new(a, b);
                doc.set_sels(vec![s], 0);
                view.drag = Some(Drag::new(DragMode::Line, s));
            } else if doc.column_mode && !ctrl && !mods.alt {
                // Column Selection Mode: click/drag places carets anywhere, even past line end
                let b = if mods.shift {
                    let mut b = doc.block.unwrap_or_else(|| doc.block_from_primary());
                    b.head_line = line;
                    b.head_col = vcol;
                    b
                } else {
                    Block::at(line, vcol)
                };
                doc.set_block(b);
                view.drag = Some(Drag::new(DragMode::Block, doc.primary_sel()));
            } else if ctrl && mods.alt && mods.shift {
                // IntelliJ: Ctrl+Alt+Shift+drag adds another rectangular fragment
                let base = doc.sels.clone();
                let mut sels = base.clone();
                sels.push(Sel::caret(pos));
                let n = sels.len() - 1;
                doc.set_sels(sels, n);
                view.drag = Some(Drag { mode: DragMode::Column { line, vcol }, origin: Sel::caret(pos), base });
            } else if mods.alt && mods.shift {
                // IntelliJ: Alt+Shift+Click adds/removes a caret, +drag adds a selection,
                // +double-click adds a word
                let before = doc.sels.len();
                add_word_or_caret(doc);
                view.drag = (doc.sels.len() >= before).then(|| Drag::new(DragMode::Char, doc.primary_sel()));
            } else if mods.alt {
                // IntelliJ: Alt+drag selects a rectangle
                doc.set_sels(vec![Sel::caret(pos)], 0);
                view.drag = Some(Drag::new(DragMode::Column { line, vcol }, Sel::caret(pos)));
            } else if ctrl {
                // Sublime: Ctrl+Click adds a caret, Ctrl+double-click adds a word
                add_word_or_caret(doc);
                view.drag = Some(Drag::new(DragMode::Char, doc.primary_sel()));
            } else if mods.shift {
                let mut s = doc.primary_sel();
                s.head = pos;
                s.goal = None;
                doc.set_sels(vec![s], 0);
                view.drag = Some(Drag::new(DragMode::Char, s));
            } else {
                let (s, mode) = match count {
                    2 => {
                        let r = double_click_range(rope, pos);
                        (Sel::new(r.start, r.end), DragMode::Word)
                    }
                    3 => {
                        let a = rope.line_to_char(line);
                        let b = if line + 1 < nlines { rope.line_to_char(line + 1) } else { rope.len_chars() };
                        (Sel::new(a, b), DragMode::Line)
                    }
                    _ => (Sel::caret(pos), DragMode::Char),
                };
                doc.set_sels(vec![s], 0);
                view.drag = Some(Drag::new(mode, s));
            }
        }
    }
    let middle_down = ui.input(|i| i.pointer.middle_down());
    if !down && !middle_down {
        view.drag = None;
        view.mm_drag = false;
        view.sb_drag = None;
    }

    // ------------------------------------------------------------ input: dragging
    if let (Some(p), Some(drag)) = (pointer, view.drag.as_ref()) {
        if resp.dragged() || down || middle_down {
            // autoscroll
            if p.y < text_rect.top() {
                view.scroll.y -= lh;
            } else if p.y > text_rect.bottom() {
                view.scroll.y += lh;
            }
            let (pos, line, vcol) = pos_at(p, doc, view.scroll);
            let rope = &doc.rope;
            let o = drag.origin;
            match drag.mode {
                DragMode::Char => {
                    let mut sels = doc.sels.clone();
                    let i = doc.primary;
                    sels[i] = Sel::new(o.anchor, pos);
                    doc.set_sels(sels, i);
                }
                DragMode::Word => {
                    let r = double_click_range(rope, pos);
                    let s = if pos < o.start() {
                        Sel::new(o.end(), r.start.min(o.start()))
                    } else {
                        Sel::new(o.start(), r.end.max(o.end()))
                    };
                    doc.set_sels(vec![s], 0);
                }
                DragMode::Line => {
                    let a = rope.line_to_char(line);
                    let b = if line + 1 < nlines { rope.line_to_char(line + 1) } else { rope.len_chars() };
                    let s = if a < o.start() { Sel::new(o.end(), a) } else { Sel::new(o.start(), b.max(o.end())) };
                    doc.set_sels(vec![s], 0);
                }
                DragMode::Column { line: l0, vcol: v0 } => {
                    let (rect, pr) = rect_sels(rope, tab, l0, v0, line, vcol);
                    let mut sels = drag.base.clone();
                    let n = sels.len() + pr;
                    sels.extend(rect);
                    doc.set_sels(sels, n);
                }
                DragMode::Block => {
                    let mut b = doc.block.unwrap_or(Block::at(line, vcol));
                    b.head_line = line;
                    b.head_col = vcol;
                    doc.set_block(b);
                }
            }
            ui.ctx().request_repaint();
        }
    }

    // minimap / scrollbar dragging
    let mm_lines = ((mm_rect.height() / MM_LH) as usize).max(1);
    let mm_first = |scroll_y: f32| -> f32 {
        if nlines <= mm_lines {
            0.0
        } else {
            let frac = if max_scroll_y > 0.0 { scroll_y / max_scroll_y } else { 0.0 };
            frac * (nlines - mm_lines) as f32
        }
    };
    if let Some(p) = pointer {
        if view.mm_drag {
            let first = mm_first(view.scroll.y);
            let line = first + (p.y - mm_rect.top()) / MM_LH;
            view.scroll.y = (line - vis_h / lh / 2.0) * lh;
        }
        if let Some(grab) = view.sb_drag {
            let (_, th) = thumb(sb_rect, view.scroll.y, max_scroll_y, vis_h);
            let track = (sb_rect.height() - th).max(1.0);
            view.scroll.y = ((p.y - sb_rect.top() - grab) / track) * max_scroll_y;
        }
    }

    // ------------------------------------------------------------ scroll to caret
    let ph = doc.primary_sel().head;
    let (pl, pc) = pos_to_lc(&doc.rope, ph);
    let pvc = doc.block.map_or_else(|| visual_col(&doc.rope, pl, pc, tab), |b| b.head_col);
    if view.center_caret {
        view.center_caret = false;
        view.scroll_to_caret = false;
        view.scroll.y = pl as f32 * lh - vis_h / 2.0;
    }
    if view.scroll_to_caret {
        view.scroll_to_caret = false;
        let y = pl as f32 * lh;
        let margin = (lh * 2.0).min(vis_h / 3.0);
        if y < view.scroll.y + margin {
            view.scroll.y = y - margin;
        } else if y + lh > view.scroll.y + vis_h - margin {
            view.scroll.y = y + lh - vis_h + margin;
        }
        let x = pvc as f32 * cw;
        let w = text_rect.width() - pad * 2.0;
        if x < view.scroll.x + cw * 4.0 {
            view.scroll.x = x - cw * 4.0;
        } else if x > view.scroll.x + w - cw * 4.0 {
            view.scroll.x = x - w + cw * 4.0;
        }
    }
    view.scroll.y = view.scroll.y.clamp(0.0, max_scroll_y);
    view.scroll.x = view.scroll.x.max(0.0);

    // ------------------------------------------------------------ painting
    let c = cx.colors;
    let rope = &doc.rope;
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, c.bg);
    painter.rect_filled(gutter, 0.0, c.gutter_bg);
    let first = (view.scroll.y / lh).floor() as usize;
    let last = (((view.scroll.y + vis_h) / lh).ceil() as usize + 1).min(nlines);
    let mm_start = mm_first(view.scroll.y) as usize;
    let mm_end = (mm_start + mm_lines).min(nlines);
    let want = if mm_w > 0.0 { last.max(mm_end) } else { last };
    if !view.cache.ensure(rope, want, cx.syn, cx.theme, Duration::from_millis(12)) {
        ui.ctx().request_repaint();
    }

    let tp = painter.with_clip_rect(text_rect);
    let y_of = |l: usize| text_rect.top() + l as f32 * lh - view.scroll.y;
    let x_of = |v: usize| text_rect.left() + pad + v as f32 * cw - view.scroll.x;
    let first_char = rope.line_to_char(first);
    let last_char = if last < nlines { rope.line_to_char(last) } else { rope.len_chars() };

    // caret lines
    let mut caret_lines: Vec<usize> = doc
        .sels
        .iter()
        .filter(|s| s.head >= first_char && s.head <= last_char)
        .map(|s| rope.char_to_line(s.head))
        .collect();
    caret_lines.dedup();
    // Current-line highlight only for carets without a selection (like IntelliJ); otherwise
    // it paints every line of a rectangular selection full-width and hides the selection.
    let any_selection =
        doc.block.is_some_and(|b| b.cols().0 != b.cols().1) || doc.sels.iter().any(|s| !s.is_empty());
    let mut hl_lines: Vec<usize> = doc
        .sels
        .iter()
        .filter(|s| !any_selection && s.head >= first_char && s.head <= last_char)
        .map(|s| rope.char_to_line(s.head))
        .collect();
    hl_lines.dedup();
    for &l in &hl_lines {
        tp.rect_filled(Rect::from_min_size(pos2(text_rect.left(), y_of(l)), vec2(text_rect.width(), lh)), 0.0, c.line_hl);
    }

    // selections
    let range_rects = |a: usize, b: usize, f: &mut dyn FnMut(Rect)| {
        let (a, b) = (a.max(first_char), b.min(last_char));
        if a > b {
            return;
        }
        let l0 = rope.char_to_line(a);
        let l1 = rope.char_to_line(b);
        for l in l0..=l1 {
            let ls = rope.line_to_char(l);
            let le = ls + line_len(rope, l);
            let sa = a.max(ls);
            let sb = b.min(le);
            if sa > le {
                continue;
            }
            let va = visual_col(rope, l, sa - ls, tab);
            let vb = visual_col(rope, l, sb - ls, tab);
            let extra = if b > le { cw * 0.7 } else { 0.0 };
            let r = Rect::from_min_max(pos2(x_of(va), y_of(l)), pos2(x_of(vb) + extra, y_of(l) + lh));
            if r.width() > 0.0 {
                f(r);
            }
        }
    };

    // diff decorations
    if let Some(d) = cx.decor {
        let row = |l: usize| Rect::from_min_size(pos2(text_rect.left(), y_of(l)), vec2(text_rect.width(), lh));
        for &(a, b, col) in &d.lines {
            for l in a.max(first)..b.min(last) {
                tp.rect_filled(row(l), 0.0, col);
            }
        }
        for &(a, b, col) in &d.inline {
            if b >= first_char && a <= last_char {
                range_rects(a, b, &mut |r| {
                    tp.rect_filled(r, 2.0, col);
                });
            }
        }
        for &(l, col) in &d.marks {
            if l >= first && l <= last {
                let y = y_of(l).round() + 0.5;
                tp.line_segment([pos2(text_rect.left(), y), pos2(text_rect.right(), y)], Stroke::new(2.0_f32, col));
            }
        }
    }

    if let Some(b) = doc.block {
        let (l0, l1) = b.lines();
        let (c0, c1) = b.cols();
        if c1 > c0 {
            for l in l0.max(first)..=l1.min(last.saturating_sub(1)) {
                let r = Rect::from_min_max(pos2(x_of(c0), y_of(l)), pos2(x_of(c1), y_of(l) + lh));
                tp.rect_filled(r, 0.0, c.selection);
            }
        }
    }
    for s in doc.sels.iter().filter(|_| doc.block.is_none()) {
        if !s.is_empty() && s.end() >= first_char && s.start() <= last_char {
            range_rects(s.start(), s.end(), &mut |r| { tp.rect_filled(r, 2.0, c.selection); });
        }
    }

    // find matches
    let fi = cx.find.partition_point(|m| m.1 < first_char);
    for m in cx.find[fi..].iter().take_while(|m| m.0 <= last_char) {
        range_rects(m.0, m.1, &mut |r| {
            tp.rect_filled(r, 2.0, c.find);
            tp.rect_stroke(r, 2.0, Stroke::new(1.0_f32, c.find.to_opaque()));
        });
    }

    // occurrences of word under caret / selected text (visible lines only)
    if doc.sels.len() == 1 && cx.find.is_empty() {
        let s = doc.primary_sel();
        let needle = if s.is_empty() {
            word_at(rope, s.head).map(|r| (rope.slice(r).to_string(), true))
        } else if s.end() - s.start() < 200 && rope.char_to_line(s.start()) == rope.char_to_line(s.end()) {
            let t = rope.slice(s.start()..s.end()).to_string();
            (!t.trim().is_empty()).then_some((t, false))
        } else {
            None
        };
        if let Some((needle, whole)) = needle {
            let nl: Vec<char> = needle.chars().collect();
            for l in first..last {
                let chars: Vec<char> = rope.line(l).chars().collect();
                let ls = rope.line_to_char(l);
                if chars.len() < nl.len() || chars.len() > 4000 {
                    continue;
                }
                let mut i = 0;
                while i + nl.len() <= chars.len() {
                    if chars[i..i + nl.len()] == nl[..] {
                        let ok = !whole
                            || ((i == 0 || !is_word_char(chars[i - 1]))
                                && chars.get(i + nl.len()).map_or(true, |c| !is_word_char(*c)));
                        if ok {
                            range_rects(ls + i, ls + i + nl.len(), &mut |r| {
                                tp.rect_stroke(r, 2.0, Stroke::new(1.0_f32, c.occurrence));
                            });
                        }
                        i += nl.len();
                    } else {
                        i += 1;
                    }
                }
            }
        }
    }

    // matching bracket
    if let Some((a, b)) = (doc.sels.len() == 1 && doc.sels[0].is_empty())
        .then(|| matching_bracket(rope, doc.sels[0].head, 200_000))
        .flatten()
    {
        for p in [a, b] {
            if p >= first_char && p < last_char {
                let (l, col) = pos_to_lc(rope, p);
                let v = visual_col(rope, l, col, tab);
                let r = Rect::from_min_size(pos2(x_of(v), y_of(l)), vec2(cw, lh));
                tp.rect_stroke(r.shrink(0.5), 2.0, Stroke::new(1.0_f32, c.bracket));
            }
        }
    }

    // text + indent guides
    let vstart = (view.scroll.x / cw).floor() as usize;
    let vend = vstart + (text_rect.width() / cw) as usize + 3;
    let ty = (lh - row_h) / 2.0;
    for l in first..last {
        let y = y_of(l);
        let spans = view.cache.spans.get(l);
        let mut job = LayoutJob::default();
        let mut buf = String::new();
        let mut cur = c.fg;
        let mut it = spans.map(|s| s.iter());
        let mut remaining = 0usize;
        let mut v = 0usize;
        let mut first_v: Option<usize> = None;
        let mut leading = true;
        let mut indent_v = 0;
        let fmt = |col: Color32| TextFormat { font_id: font.clone(), color: col, ..Default::default() };
        for ch in rope.line(l).chars() {
            if ch == '\n' {
                break;
            }
            if remaining == 0 {
                let (n, col) = it
                    .as_mut()
                    .and_then(|i| i.next())
                    .copied()
                    .unwrap_or((usize::MAX, c.fg));
                remaining = n;
                if col != cur && !buf.is_empty() {
                    job.append(&buf, 0.0, fmt(cur));
                    buf.clear();
                }
                cur = col;
            }
            remaining = remaining.saturating_sub(1);
            let w = if ch == '\t' { tab - v % tab } else { 1 };
            if leading {
                if ch == ' ' || ch == '\t' {
                    indent_v = v + w;
                } else {
                    leading = false;
                }
            }
            if v + w > vstart && v < vend {
                if first_v.is_none() {
                    first_v = Some(v);
                }
                if ch == '\t' {
                    buf.extend(std::iter::repeat(' ').take(w));
                } else {
                    buf.push(ch);
                }
            }
            v += w;
            if v >= vend && !leading {
                break;
            }
        }
        if !buf.is_empty() {
            job.append(&buf, 0.0, fmt(cur));
        }
        // indent guides
        let mut g = tab;
        while g < indent_v {
            let x = x_of(g) + 0.5;
            tp.line_segment([pos2(x, y), pos2(x, y + lh)], Stroke::new(1.0_f32, c.guide));
            g += tab;
        }
        if let Some(fv) = first_v {
            let galley = ui.fonts(|f| f.layout_job(job));
            tp.galley(pos2(x_of(fv), y + ty), galley, c.fg);
        }
    }

    // carets
    let focused = resp.has_focus();
    let sig = (doc.version, doc.primary_sel().head, doc.sels.len());
    if sig != view.blink_sig {
        view.blink_sig = sig;
        view.blink_epoch = time;
    }
    let phase = (time - view.blink_epoch) % 1.1;
    let blink_on = !focused || phase < 0.6;
    if focused {
        let next = if phase < 0.6 { 0.6 - phase } else { 1.1 - phase };
        ui.ctx().request_repaint_after(Duration::from_secs_f64(next.max(0.01)));
    }
    if blink_on {
        let caret_col = if focused { c.caret } else { c.caret.gamma_multiply(0.35) };
        let caret = |l: usize, v: usize| {
            tp.rect_filled(Rect::from_min_size(pos2(x_of(v) - 1.0, y_of(l) + 1.0), vec2(2.0, lh - 2.0)), 0.0, caret_col);
        };
        if let Some(b) = doc.block {
            // carets sit at the block column on every line, even in virtual space
            let (l0, l1) = b.lines();
            for l in l0.max(first)..=l1.min(last.saturating_sub(1)) {
                caret(l, b.head_col);
            }
        } else {
            for s in &doc.sels {
                if s.head < first_char || s.head > last_char {
                    continue;
                }
                let (l, col) = pos_to_lc(rope, s.head);
                caret(l, visual_col(rope, l, col, tab));
            }
        }
    }

    // gutter
    let gp = painter.with_clip_rect(gutter);
    for l in first..last {
        let active = caret_lines.contains(&l);
        gp.text(
            pos2(gutter.right() - cw * 1.5, y_of(l) + lh / 2.0),
            Align2::RIGHT_CENTER,
            (l + 1).to_string(),
            font.clone(),
            if active { c.gutter_fg_active } else { c.gutter_fg },
        );
    }

    // scrollbar
    let (ty0, th) = thumb(sb_rect, view.scroll.y, max_scroll_y, vis_h);
    let sb_hover = pointer.is_some_and(|p| sb_rect.contains(p)) || view.sb_drag.is_some();
    painter.rect_filled(
        Rect::from_min_size(pos2(sb_rect.left() + 3.0, ty0), vec2(SB_W - 6.0, th)),
        3.0,
        c.fg.gamma_multiply(if sb_hover { 0.35 } else { 0.15 }),
    );
    // carets/matches markers on the scrollbar
    if nlines > 1 {
        let mark = |line: usize, col: Color32| {
            let y = sb_rect.top() + line as f32 / nlines as f32 * sb_rect.height();
            painter.rect_filled(Rect::from_min_size(pos2(sb_rect.left() + 2.0, y), vec2(SB_W - 4.0, 2.0)), 0.0, col);
        };
        for m in cx.find.iter().take(5000) {
            mark(rope.char_to_line(m.0), c.find.to_opaque());
        }
        if let Some(d) = cx.decor {
            for &(a, _, col) in d.lines.iter().take(5000) {
                mark(a, col.to_opaque());
            }
            for &(l, col) in d.marks.iter().take(5000) {
                mark(l, col.to_opaque());
            }
        }
        if doc.sels.len() < 5000 {
            for s in &doc.sels {
                mark(rope.char_to_line(s.head), c.caret.gamma_multiply(0.7));
            }
        }
    }

    // minimap
    if mm_w > 0.0 {
        let mp = painter.with_clip_rect(mm_rect);
        mp.rect_filled(mm_rect, 0.0, c.bg);
        mp.line_segment(
            [pos2(mm_rect.left(), mm_rect.top()), pos2(mm_rect.left(), mm_rect.bottom())],
            Stroke::new(1.0_f32, c.guide),
        );
        let mm_f = mm_first(view.scroll.y);
        let base = mm_f.floor() as usize;
        let max_v = ((mm_w - 8.0) / MM_CW) as usize;
        for l in base..mm_end.min(base + mm_lines + 1) {
            let y = mm_rect.top() + (l as f32 - mm_f) * MM_LH;
            let spans = view.cache.spans.get(l);
            let mut it = spans.map(|s| s.iter());
            let mut remaining = 0usize;
            let mut cur = c.fg;
            let mut v = 0usize;
            let mut run: Option<(usize, Color32)> = None;
            let flush = |run: &mut Option<(usize, Color32)>, v: usize| {
                if let Some((s, col)) = run.take() {
                    let x = mm_rect.left() + 6.0 + s as f32 * MM_CW;
                    mp.rect_filled(
                        Rect::from_min_size(pos2(x, y), vec2((v - s) as f32 * MM_CW, MM_LH - 1.0)),
                        0.0,
                        col.gamma_multiply(0.75),
                    );
                }
            };
            for ch in rope.line(l).chars() {
                if ch == '\n' || v > max_v {
                    break;
                }
                if remaining == 0 {
                    let (n, col) = it.as_mut().and_then(|i| i.next()).copied().unwrap_or((usize::MAX, c.fg));
                    remaining = n;
                    cur = col;
                }
                remaining = remaining.saturating_sub(1);
                let w = if ch == '\t' { tab - v % tab } else { 1 };
                if ch.is_whitespace() || run.is_some_and(|r| r.1 != cur) {
                    flush(&mut run, v);
                }
                if !ch.is_whitespace() && run.is_none() {
                    run = Some((v, cur));
                }
                v += w;
            }
            flush(&mut run, v.min(max_v));
        }
        let vy = mm_rect.top() + (view.scroll.y / lh - mm_f) * MM_LH;
        let vr = Rect::from_min_size(pos2(mm_rect.left(), vy), vec2(mm_w, (vis_h / lh) * MM_LH));
        let mm_hover = pointer.is_some_and(|p| mm_rect.contains(p)) || view.mm_drag;
        mp.rect_filled(vr, 0.0, c.fg.gamma_multiply(if mm_hover { 0.12 } else { 0.06 }));
    }

    if resp.hovered() && pointer.is_some_and(|p| text_rect.contains(p)) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }
    ui.allocate_rect(rect, Sense::hover());
    resp
}

fn thumb(sb: Rect, scroll_y: f32, max_scroll: f32, vis_h: f32) -> (f32, f32) {
    let content = max_scroll + vis_h;
    let th = (sb.height() * vis_h / content.max(1.0)).clamp(24.0, sb.height());
    let frac = if max_scroll > 0.0 { scroll_y / max_scroll } else { 0.0 };
    (sb.top() + frac * (sb.height() - th), th)
}
