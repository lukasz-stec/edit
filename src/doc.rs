//! A document: rope + multi-selection + undo history + all editing commands.

use crate::search;
use crate::selection::{normalize, same_ranges, Sel};
use crate::text::*;
use ropey::Rope;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditKind {
    Insert,
    Delete,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    Home,
    End,
    DocStart,
    DocEnd,
    PageUp(usize),
    PageDown(usize),
}

/// A rectangle in *visual* columns, used by Column Selection Mode (IntelliJ Alt+Shift+Insert).
/// Unlike plain selections it may extend past the end of short lines ("virtual space");
/// typing there pads the line with spaces.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Block {
    pub anchor_line: usize,
    pub anchor_col: usize,
    pub head_line: usize,
    pub head_col: usize,
}

impl Block {
    pub fn at(line: usize, col: usize) -> Self {
        Block { anchor_line: line, anchor_col: col, head_line: line, head_col: col }
    }
    pub fn lines(&self) -> (usize, usize) {
        (self.anchor_line.min(self.head_line), self.anchor_line.max(self.head_line))
    }
    pub fn cols(&self) -> (usize, usize) {
        (self.anchor_col.min(self.head_col), self.anchor_col.max(self.head_col))
    }
}

/// One selection per line covering visual columns `a_col..h_col` (clamped to each line's end).
/// Returns the selections and the index of the one on the head line.
pub fn rect_sels(rope: &Rope, tab: usize, a_line: usize, a_col: usize, h_line: usize, h_col: usize) -> (Vec<Sel>, usize) {
    let (la, lb) = (a_line.min(h_line), a_line.max(h_line));
    let sels = (la..=lb)
        .map(|l| {
            let ls = rope.line_to_char(l);
            Sel {
                anchor: ls + col_from_visual(rope, l, a_col, tab),
                head: ls + col_from_visual(rope, l, h_col, tab),
                goal: Some(h_col),
            }
        })
        .collect();
    (sels, if h_line >= a_line { lb - la } else { 0 })
}

fn visual_len(rope: &Rope, line: usize, tab: usize) -> usize {
    visual_col(rope, line, line_len(rope, line), tab)
}

#[derive(Clone)]
struct Snapshot {
    rope: Rope,
    sels: Vec<Sel>,
    primary: usize,
    version: u64,
}

/// Replace `start..end` with `text`; the resulting selection is `anchor`/`head`
/// as offsets from `start` in the new text.
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub anchor: usize,
    pub head: usize,
}

impl Edit {
    pub fn replace(start: usize, end: usize, text: impl Into<String>) -> Self {
        let text = text.into();
        let n = text.chars().count();
        Edit { start, end, text, anchor: n, head: n }
    }
    fn caret_at(pos: usize, off: usize) -> Self {
        Edit { start: pos, end: pos, text: String::new(), anchor: off, head: off }
    }
}

type LineMap = Box<dyn Fn(usize, usize) -> (usize, usize)>;
fn lm(f: impl Fn(usize, usize) -> (usize, usize) + 'static) -> LineMap {
    Box::new(f)
}

pub struct Document {
    pub id: u64,
    pub rope: Rope,
    pub path: Option<PathBuf>,
    pub sels: Vec<Sel>,
    pub primary: usize,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_kind: EditKind,
    last_time: Instant,
    pub version: u64,
    next_version: u64,
    saved_version: u64,
    pub crlf: bool,
    pub use_tabs: bool,
    pub tab_size: usize,
    /// Lowest line changed since the highlighter last looked.
    pub dirty_line: Option<usize>,
    expand_stack: Vec<(Vec<Sel>, Vec<Sel>)>,
    occ_whole_word: bool,
    pub camel_humps: bool,
    /// IntelliJ Column Selection Mode (per tab, not persisted).
    pub column_mode: bool,
    /// Active rectangular block; `sels` mirror it (clamped to line ends).
    pub block: Option<Block>,
}

fn pair_close(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => Some('"'),
        '\'' => Some('\''),
        '`' => Some('`'),
        _ => None,
    }
}

fn first_diff_line(a: &Rope, b: &Rope) -> usize {
    let mut i = 0;
    for (x, y) in a.bytes().zip(b.bytes()) {
        if x != y {
            break;
        }
        i += 1;
    }
    let i = i.min(a.len_bytes());
    a.char_to_line(a.byte_to_char(i))
}

impl Document {
    pub fn new() -> Self {
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            rope: Rope::new(),
            path: None,
            sels: vec![Sel::caret(0)],
            primary: 0,
            undo: vec![],
            redo: vec![],
            last_kind: EditKind::Other,
            last_time: Instant::now(),
            version: 0,
            next_version: 1,
            saved_version: 0,
            crlf: false,
            use_tabs: false,
            tab_size: 4,
            dirty_line: None,
            expand_stack: vec![],
            occ_whole_word: false,
            camel_humps: true,
            column_mode: false,
            block: None,
        }
    }

    #[allow(dead_code)]
    pub fn from_str(text: &str) -> Self {
        let mut d = Self::new();
        d.set_text(text);
        d
    }

    fn set_text(&mut self, text: &str) {
        let mut text = text.strip_prefix('\u{feff}').unwrap_or(text).to_string();
        self.crlf = text.contains("\r\n");
        if self.crlf {
            text = text.replace("\r\n", "\n");
        }
        let (mut tabs, mut spaces) = (0, 0);
        for line in text.lines().take(2000) {
            if line.starts_with('\t') {
                tabs += 1;
            } else if line.starts_with("  ") {
                spaces += 1;
            }
        }
        self.use_tabs = tabs > spaces;
        self.rope = Rope::from_str(&text);
    }

    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut d = Self::new();
        if path.exists() {
            let bytes = std::fs::read(path)?;
            d.set_text(&String::from_utf8_lossy(&bytes));
        }
        d.path = Some(path.to_path_buf());
        Ok(d)
    }

    pub fn save_to(&mut self, path: &Path) -> std::io::Result<()> {
        let mut s = self.rope.to_string();
        if self.crlf {
            s = s.replace('\n', "\r\n");
        }
        std::fs::write(path, s)?;
        self.path = Some(path.to_path_buf());
        self.saved_version = self.version;
        Ok(())
    }

    pub fn is_dirty(&self) -> bool {
        self.version != self.saved_version
    }

    pub fn is_pristine(&self) -> bool {
        self.path.is_none() && self.rope.len_chars() == 0 && !self.is_dirty()
    }

    pub fn name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".into())
    }

    pub fn len(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn primary_sel(&self) -> Sel {
        self.sels[self.primary]
    }

    pub fn indent_unit(&self) -> String {
        if self.use_tabs {
            "\t".into()
        } else {
            " ".repeat(self.tab_size)
        }
    }

    pub fn comment_token(&self) -> &'static str {
        let ext = self
            .path
            .as_ref()
            .and_then(|p| p.extension())
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "py" | "sh" | "bash" | "zsh" | "rb" | "toml" | "yaml" | "yml" | "ps1" | "pl" | "r"
            | "conf" | "cfg" | "properties" | "dockerfile" | "make" | "mk" | "cmake" | "nim"
            | "ex" | "exs" | "jl" | "tf" | "gitignore" => "#",
            "sql" | "lua" | "hs" | "elm" | "ada" => "--",
            "tex" | "erl" | "m" => "%",
            "ini" | "asm" | "lisp" | "clj" | "el" | "scm" => ";",
            "vim" => "\"",
            "bat" | "cmd" => "REM",
            _ => "//",
        }
    }

    // ------------------------------------------------------------ core mutation

    /// Replace the selections; merges overlaps and breaks the undo group.
    pub fn set_sels(&mut self, sels: Vec<Sel>, primary: usize) {
        self.sels = sels;
        self.primary = primary;
        normalize(&mut self.sels, &mut self.primary, self.rope.len_chars());
        self.last_kind = EditKind::Other;
        self.occ_whole_word = false;
        self.block = None;
    }

    // ------------------------------------------------------------ column block

    pub fn set_block(&mut self, b: Block) {
        let last = self.rope.len_lines() - 1;
        let b = Block { anchor_line: b.anchor_line.min(last), head_line: b.head_line.min(last), ..b };
        let (sels, p) = rect_sels(&self.rope, self.tab_size, b.anchor_line, b.anchor_col, b.head_line, b.head_col);
        self.set_sels(sels, p);
        self.block = Some(b);
    }

    /// Re-derive selections from the block without breaking the undo group.
    fn refresh_block_sels(&mut self) {
        if let Some(b) = self.block {
            let (sels, p) = rect_sels(&self.rope, self.tab_size, b.anchor_line, b.anchor_col, b.head_line, b.head_col);
            self.sels = sels;
            self.primary = p;
            normalize(&mut self.sels, &mut self.primary, self.rope.len_chars());
        }
    }

    /// Block spanning the primary selection (anchor corner to head corner).
    pub fn block_from_primary(&self) -> Block {
        let s = self.primary_sel();
        let tab = self.tab_size;
        let (al, ac) = pos_to_lc(&self.rope, s.anchor);
        let (hl, hc) = pos_to_lc(&self.rope, s.head);
        let hv = s.goal.filter(|_| s.is_empty()).unwrap_or_else(|| visual_col(&self.rope, hl, hc, tab));
        let av = if s.is_empty() { hv } else { visual_col(&self.rope, al, ac, tab) };
        Block { anchor_line: al, anchor_col: av, head_line: hl, head_col: hv }
    }

    pub fn toggle_column_mode(&mut self) {
        self.column_mode = !self.column_mode;
        if self.column_mode {
            if self.sels.len() == 1 {
                let b = self.block_from_primary();
                self.set_block(b);
            }
        } else {
            self.block = None;
        }
    }

    /// Replace the block's rectangle on every line, padding short lines with spaces.
    /// `texts` is either one text for all lines or one per line. Leaves a zero-width
    /// block (carets) right after the inserted text.
    pub fn block_replace(&mut self, texts: &[String], kind: EditKind) {
        let Some(b) = self.block else { return };
        let (l0, l1) = b.lines();
        let (c0, c1) = b.cols();
        let tab = self.tab_size;
        let per_line = texts.len() == l1 - l0 + 1 && texts.len() > 1;
        let rope = &self.rope;
        let mut edits = vec![];
        for (i, l) in (l0..=l1).enumerate() {
            let t = if per_line { &texts[i] } else { &texts[0] };
            let ls = rope.line_to_char(l);
            let len = line_len(rope, l);
            let vl = visual_len(rope, l, tab);
            if vl < c0 {
                if !t.is_empty() {
                    edits.push(Edit::replace(ls + len, ls + len, format!("{}{t}", " ".repeat(c0 - vl))));
                }
            } else {
                let a = col_from_visual(rope, l, c0, tab);
                let e = col_from_visual(rope, l, c1, tab).max(a);
                edits.push(Edit::replace(ls + a, ls + e, t.clone()));
            }
        }
        let w = texts[0].chars().count();
        let uniform = !per_line || texts.iter().all(|t| t.chars().count() == w);
        self.apply(kind, edits);
        if uniform {
            let nc = c0 + w;
            self.block = Some(Block { anchor_line: b.anchor_line, anchor_col: nc, head_line: b.head_line, head_col: nc });
            self.refresh_block_sels();
        } else {
            self.block = None;
        }
    }

    pub fn block_backspace(&mut self) {
        let Some(b) = self.block else { return };
        let (c0, c1) = b.cols();
        if c0 != c1 {
            return self.block_replace(&[String::new()], EditKind::Delete);
        }
        if c0 == 0 {
            return;
        }
        let (l0, l1) = b.lines();
        let tab = self.tab_size;
        let rope = &self.rope;
        let edits: Vec<Edit> = (l0..=l1)
            .filter(|&l| visual_len(rope, l, tab) >= c0)
            .filter_map(|l| {
                let e = col_from_visual(rope, l, c0, tab);
                let ls = rope.line_to_char(l);
                (e > 0).then(|| Edit::replace(ls + e - 1, ls + e, ""))
            })
            .collect();
        self.apply(EditKind::Delete, edits);
        self.block = Some(Block { anchor_col: c0 - 1, head_col: c0 - 1, ..b });
        self.refresh_block_sels();
    }

    pub fn block_delete(&mut self) {
        let Some(b) = self.block else { return };
        let (c0, c1) = b.cols();
        if c0 != c1 {
            return self.block_replace(&[String::new()], EditKind::Delete);
        }
        let (l0, l1) = b.lines();
        let tab = self.tab_size;
        let rope = &self.rope;
        let edits: Vec<Edit> = (l0..=l1)
            .filter(|&l| visual_len(rope, l, tab) > c0)
            .map(|l| {
                let ls = rope.line_to_char(l) + col_from_visual(rope, l, c0, tab);
                Edit::replace(ls, ls + 1, "")
            })
            .collect();
        self.apply(EditKind::Delete, edits);
        self.block = Some(b);
        self.refresh_block_sels();
    }

    /// Text of the rectangle, one line per row; virtual space is copied as spaces.
    pub fn block_copy(&self) -> String {
        let Some(b) = self.block else { return String::new() };
        let (l0, l1) = b.lines();
        let (c0, c1) = b.cols();
        let tab = self.tab_size;
        (l0..=l1)
            .map(|l| {
                let mut s = String::new();
                let mut v = 0;
                for ch in self.rope.line(l).chars() {
                    if ch == '\n' || v >= c1 {
                        break;
                    }
                    let w = if ch == '\t' { tab - v % tab } else { 1 };
                    if v >= c0 {
                        if ch == '\t' {
                            s.extend(std::iter::repeat(' ').take(w.min(c1 - v)));
                        } else {
                            s.push(ch);
                        }
                    }
                    v += w;
                }
                let have = s.chars().count();
                s + &" ".repeat((c1 - c0).saturating_sub(have))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Arrow keys in column mode. `extend` (Shift) grows the rectangle from its anchor;
    /// otherwise the carets move together (collapsing a non-empty block first).
    pub fn block_move(&mut self, dl: isize, dc: isize, extend: bool) {
        let b = self.block.unwrap_or_else(|| self.block_from_primary());
        let nl = self.rope.len_lines() as isize;
        let mv = |l: usize, d: isize| (l as isize + d).clamp(0, nl - 1) as usize;
        let mc = |c: usize, d: isize| (c as isize + d).max(0) as usize;
        let nb = if extend {
            Block { head_line: mv(b.head_line, dl), head_col: mc(b.head_col, dc), ..b }
        } else {
            let (c0, c1) = b.cols();
            if c0 != c1 && dc != 0 {
                let c = if dc < 0 { c0 } else { c1 };
                Block { anchor_col: c, head_col: c, ..b }
            } else {
                let (l0, l1) = b.lines();
                let dl = dl.clamp(-(l0 as isize), nl - 1 - l1 as isize);
                Block {
                    anchor_line: mv(b.anchor_line, dl),
                    head_line: mv(b.head_line, dl),
                    anchor_col: mc(b.anchor_col, dc),
                    head_col: mc(b.head_col, dc),
                }
            }
        };
        self.set_block(nb);
    }

    /// IntelliJ Alt+Shift+G: a caret at the end of every line touched by a selection.
    pub fn carets_at_line_ends(&mut self) {
        let mut out = vec![];
        for s in &self.sels {
            if s.is_empty() {
                out.push(*s);
                continue;
            }
            let (l0, l1) = self.sel_line_range(s);
            out.extend((l0..=l1).map(|l| Sel::caret(line_end(&self.rope, l))));
        }
        let n = out.len();
        self.set_sels(out, n.saturating_sub(1));
    }

    fn checkpoint(&mut self, kind: EditKind) {
        let now = Instant::now();
        let group = kind != EditKind::Other
            && kind == self.last_kind
            && now.duration_since(self.last_time) < Duration::from_millis(1500);
        if !group {
            self.undo.push(Snapshot {
                rope: self.rope.clone(),
                sels: self.sels.clone(),
                primary: self.primary,
                version: self.version,
            });
            if self.undo.len() > 5000 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last_kind = kind;
        self.last_time = now;
    }

    fn bump(&mut self, line: usize) {
        self.version = self.next_version;
        self.next_version += 1;
        self.dirty_line = Some(self.dirty_line.map_or(line, |d| d.min(line)));
    }

    /// Apply non-overlapping edits (one per selection, in selection order).
    pub fn apply(&mut self, kind: EditKind, mut edits: Vec<Edit>) {
        let changes = edits.iter().any(|e| e.start != e.end || !e.text.is_empty());
        if changes {
            self.checkpoint(kind);
        }
        edits.sort_by_key(|e| e.start);
        let mut delta: isize = 0;
        let mut last_end = 0usize;
        let mut min_line = usize::MAX;
        let mut new_sels = Vec::with_capacity(edits.len());
        for e in edits {
            let s = e.start.max(last_end);
            let en = e.end.max(s);
            last_end = en;
            let ns = (s as isize + delta) as usize;
            if en > s || !e.text.is_empty() {
                min_line = min_line.min(self.rope.char_to_line(ns));
            }
            if en > s {
                self.rope.remove(ns..ns + (en - s));
            }
            let n = e.text.chars().count();
            if n > 0 {
                self.rope.insert(ns, &e.text);
            }
            delta += n as isize - (en - s) as isize;
            new_sels.push(Sel::new(ns + e.anchor, ns + e.head));
        }
        let prim = self.primary.min(new_sels.len().saturating_sub(1));
        self.sels = new_sels;
        self.primary = prim;
        let len = self.rope.len_chars();
        normalize(&mut self.sels, &mut self.primary, len);
        if changes {
            self.bump(min_line);
        }
    }

    pub fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            let cur = self.snapshot();
            self.redo.push(cur);
            self.restore(s);
        }
    }

    pub fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            let cur = self.snapshot();
            self.undo.push(cur);
            self.restore(s);
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { rope: self.rope.clone(), sels: self.sels.clone(), primary: self.primary, version: self.version }
    }

    fn restore(&mut self, s: Snapshot) {
        let line = first_diff_line(&self.rope, &s.rope);
        self.rope = s.rope;
        self.sels = s.sels;
        self.primary = s.primary;
        self.version = s.version;
        self.dirty_line = Some(self.dirty_line.map_or(line, |d| d.min(line)));
        self.last_kind = EditKind::Other;
        self.block = None;
    }

    // ------------------------------------------------------------ movement

    pub fn move_sels(&mut self, m: Motion, extend: bool) {
        let rope = &self.rope;
        let (tab, camel) = (self.tab_size, self.camel_humps);
        let new: Vec<Sel> = self
            .sels
            .iter()
            .map(|s| {
                if !extend && !s.is_empty() {
                    match m {
                        Motion::Left => return Sel::caret(s.start()),
                        Motion::Right => return Sel::caret(s.end()),
                        _ => {}
                    }
                }
                let (head, goal) = motion_target(rope, s, m, tab, camel);
                Sel { anchor: if extend { s.anchor } else { head }, head, goal }
            })
            .collect();
        let p = self.primary;
        self.set_sels(new, p);
    }

    // ------------------------------------------------------------ typing

    pub fn type_text(&mut self, text: &str) {
        let text = text.replace('\r', "");
        if text.is_empty() {
            return;
        }
        let mut it = text.chars();
        let single = match (it.next(), it.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        };
        let rope = &self.rope;
        let len = rope.len_chars();
        let edits: Vec<Edit> = self
            .sels
            .iter()
            .map(|s| {
                let (st, en) = (s.start(), s.end());
                if let Some(c) = single {
                    let next = if en < len { Some(rope.char(en)) } else { None };
                    let prev = if st > 0 { Some(rope.char(st - 1)) } else { None };
                    if let Some(close) = pair_close(c) {
                        if !s.is_empty() {
                            // wrap selection
                            let inner = rope.slice(st..en).to_string();
                            let n = inner.chars().count();
                            let (a, h) = if s.reversed() { (n + 1, 1) } else { (1, n + 1) };
                            return Edit { start: st, end: en, text: format!("{c}{inner}{close}"), anchor: a, head: h };
                        }
                        let is_quote = c == close;
                        if is_quote && next == Some(c) {
                            return Edit::caret_at(st, 1);
                        }
                        let next_ok = next.map_or(true, |n| n.is_whitespace() || ")]},;:>".contains(n));
                        let prev_ok = !is_quote
                            || prev.map_or(true, |p| !(is_word_char(p) || p == c || p == '\\'));
                        if next_ok && prev_ok {
                            return Edit { start: st, end: en, text: format!("{c}{close}"), anchor: 1, head: 1 };
                        }
                    } else if matches!(c, ')' | ']' | '}') && s.is_empty() && next == Some(c) {
                        return Edit::caret_at(st, 1);
                    }
                }
                Edit::replace(st, en, text.clone())
            })
            .collect();
        self.apply(EditKind::Insert, edits);
    }

    pub fn newline(&mut self) {
        let unit = self.indent_unit();
        let rope = &self.rope;
        let len = rope.len_chars();
        let edits: Vec<Edit> = self
            .sels
            .iter()
            .map(|s| {
                let (st, en) = (s.start(), s.end());
                let (l, c) = pos_to_lc(rope, st);
                let indent: String =
                    rope.line(l).chars().take(c).take_while(|ch| *ch == ' ' || *ch == '\t').collect();
                let prev = if st > 0 { Some(rope.char(st - 1)) } else { None };
                let next = if en < len { Some(rope.char(en)) } else { None };
                match (prev, next) {
                    (Some(p), Some(n)) if "([{".contains(p) && pair_close(p) == Some(n) => {
                        let k = 1 + indent.chars().count() + unit.chars().count();
                        Edit { start: st, end: en, text: format!("\n{indent}{unit}\n{indent}"), anchor: k, head: k }
                    }
                    (Some(p), _) if "([{".contains(p) => Edit::replace(st, en, format!("\n{indent}{unit}")),
                    _ => Edit::replace(st, en, format!("\n{indent}")),
                }
            })
            .collect();
        self.apply(EditKind::Other, edits);
    }

    pub fn tab(&mut self) {
        if self.sels.iter().any(|s| !s.is_empty()) {
            return self.indent();
        }
        let (tab, use_tabs) = (self.tab_size, self.use_tabs);
        let rope = &self.rope;
        let edits: Vec<Edit> = self
            .sels
            .iter()
            .map(|s| {
                let (l, c) = pos_to_lc(rope, s.head);
                let v = visual_col(rope, l, c, tab);
                let t = if use_tabs { "\t".to_string() } else { " ".repeat(tab - v % tab) };
                Edit::replace(s.start(), s.end(), t)
            })
            .collect();
        self.apply(EditKind::Insert, edits);
    }

    pub fn backspace(&mut self, word: bool) {
        let rope = &self.rope;
        let len = rope.len_chars();
        let (tab, use_tabs, camel) = (self.tab_size, self.use_tabs, self.camel_humps);
        let edits: Vec<Edit> = self
            .sels
            .iter()
            .map(|s| {
                if !s.is_empty() {
                    return Edit::replace(s.start(), s.end(), "");
                }
                let p = s.head;
                if p == 0 {
                    return Edit::replace(0, 0, "");
                }
                if word {
                    return Edit::replace(prev_word(rope, p, camel), p, "");
                }
                let prev = rope.char(p - 1);
                let next = if p < len { Some(rope.char(p)) } else { None };
                if pair_close(prev).is_some() && pair_close(prev) == next {
                    return Edit::replace(p - 1, p + 1, "");
                }
                let (l, c) = pos_to_lc(rope, p);
                if !use_tabs && c > 0 && rope.line(l).chars().take(c).all(|ch| ch == ' ') {
                    let n = ((c - 1) % tab) + 1;
                    return Edit::replace(p - n, p, "");
                }
                Edit::replace(p - 1, p, "")
            })
            .collect();
        self.apply(EditKind::Delete, edits);
    }

    pub fn delete_forward(&mut self, word: bool) {
        let rope = &self.rope;
        let len = rope.len_chars();
        let camel = self.camel_humps;
        let edits: Vec<Edit> = self
            .sels
            .iter()
            .map(|s| {
                if !s.is_empty() {
                    return Edit::replace(s.start(), s.end(), "");
                }
                let p = s.head;
                let e = if word { next_word(rope, p, camel) } else { (p + 1).min(len) };
                Edit::replace(p, e, "")
            })
            .collect();
        self.apply(EditKind::Delete, edits);
    }

    pub fn delete_selections(&mut self) {
        let edits = self.sels.iter().map(|s| Edit::replace(s.start(), s.end(), "")).collect();
        self.apply(EditKind::Delete, edits);
    }

    /// Replace every selection with `text` (used by replace-all and paste).
    pub fn replace_selections(&mut self, text: &str) {
        let edits = self.sels.iter().map(|s| Edit::replace(s.start(), s.end(), text)).collect();
        self.apply(EditKind::Other, edits);
    }

    pub fn insert_line(&mut self, above: bool) {
        let rope = &self.rope;
        let edits: Vec<Edit> = self
            .sels
            .iter()
            .map(|s| {
                let l = rope.char_to_line(s.head);
                let indent = leading_ws(rope, l);
                let n = indent.chars().count();
                if above {
                    let p = rope.line_to_char(l);
                    Edit { start: p, end: p, text: format!("{indent}\n"), anchor: n, head: n }
                } else {
                    let p = line_end(rope, l);
                    Edit { start: p, end: p, text: format!("\n{indent}"), anchor: n + 1, head: n + 1 }
                }
            })
            .collect();
        self.apply(EditKind::Other, edits);
    }

    pub fn toggle_case(&mut self) {
        let rope = &self.rope;
        let edits: Vec<Edit> = self
            .sels
            .iter()
            .map(|s| {
                let r = if s.is_empty() {
                    word_at(rope, s.head).unwrap_or(s.head..s.head)
                } else {
                    s.start()..s.end()
                };
                let t = rope.slice(r.clone()).to_string();
                let nt = if t.chars().any(|c| c.is_lowercase()) { t.to_uppercase() } else { t.to_lowercase() };
                let n = nt.chars().count();
                let (a, h) = if s.reversed() { (n, 0) } else { (0, n) };
                Edit { start: r.start, end: r.end, text: nt, anchor: a, head: h }
            })
            .collect();
        self.apply(EditKind::Other, edits);
    }

    // ------------------------------------------------------------ clipboard

    /// Returns (text, whole_line_mode). With no selection, copies whole lines (like Sublime/IntelliJ).
    pub fn copy(&self) -> (String, bool) {
        let rope = &self.rope;
        if self.sels.iter().all(|s| s.is_empty()) {
            let mut out = String::new();
            let mut last = usize::MAX;
            for s in &self.sels {
                let l = rope.char_to_line(s.head);
                if l == last {
                    continue;
                }
                last = l;
                out.push_str(&line_text(rope, l));
                out.push('\n');
            }
            (out, true)
        } else {
            let parts: Vec<String> = self
                .sels
                .iter()
                .filter(|s| !s.is_empty())
                .map(|s| rope.slice(s.start()..s.end()).to_string())
                .collect();
            (parts.join("\n"), false)
        }
    }

    pub fn cut(&mut self) -> (String, bool) {
        let r = self.copy();
        if r.1 {
            self.delete_lines();
        } else {
            self.delete_selections();
        }
        r
    }

    pub fn paste(&mut self, text: &str, line_mode: bool) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let rope = &self.rope;
        if line_mode && self.sels.iter().all(|s| s.is_empty()) {
            let lines: Vec<usize> = self.sels.iter().map(|s| rope.char_to_line(s.head)).collect();
            if lines.windows(2).all(|w| w[0] != w[1]) {
                let n = text.chars().count();
                let edits = self
                    .sels
                    .iter()
                    .zip(&lines)
                    .map(|(s, &l)| {
                        let ls = rope.line_to_char(l);
                        let off = n + (s.head - ls);
                        Edit { start: ls, end: ls, text: text.clone(), anchor: off, head: off }
                    })
                    .collect();
                return self.apply(EditKind::Other, edits);
            }
        }
        let trimmed = text.strip_suffix('\n').unwrap_or(&text);
        let parts: Vec<&str> = trimmed.split('\n').collect();
        if self.sels.len() > 1 && parts.len() == self.sels.len() {
            let edits = self
                .sels
                .iter()
                .zip(parts)
                .map(|(s, p)| Edit::replace(s.start(), s.end(), p))
                .collect();
            return self.apply(EditKind::Other, edits);
        }
        self.replace_selections(&text);
    }

    // ------------------------------------------------------------ selection commands

    pub fn select_all(&mut self) {
        let n = self.len();
        self.set_sels(vec![Sel::new(0, n)], 0);
    }

    pub fn single_selection(&mut self) {
        let p = self.primary_sel();
        let s = if self.sels.len() > 1 { p } else { Sel::caret(p.head) };
        self.set_sels(vec![s], 0);
    }

    /// Ctrl+L: select the line; repeat to extend downward.
    pub fn select_line(&mut self) {
        let rope = &self.rope;
        let len = rope.len_chars();
        let new: Vec<Sel> = self
            .sels
            .iter()
            .map(|s| {
                let l0 = rope.char_to_line(s.start());
                let l1 = rope.char_to_line(s.end());
                let e = if l1 + 1 < rope.len_lines() { rope.line_to_char(l1 + 1) } else { len };
                Sel::new(rope.line_to_char(l0), e)
            })
            .collect();
        let p = self.primary;
        self.set_sels(new, p);
    }

    /// IntelliJ "Extend Selection" (Ctrl+W): hump -> word -> string -> line -> block -> file.
    pub fn expand_selection(&mut self) {
        let before = self.sels.clone();
        let rope = &self.rope;
        let camel = self.camel_humps;
        let new: Vec<Sel> = self
            .sels
            .iter()
            .map(|s| {
                let (st, en) = (s.start(), s.end());
                expand_candidates(rope, st, en, camel)
                    .into_iter()
                    .filter(|r| r.start <= st && r.end >= en && r.end - r.start > en - st)
                    .min_by_key(|r| r.end - r.start)
                    .map(|r| Sel::new(r.start, r.end))
                    .unwrap_or(*s)
            })
            .collect();
        if same_ranges(&new, &before) {
            return;
        }
        if let Some((_, after)) = self.expand_stack.last() {
            if !same_ranges(after, &before) {
                self.expand_stack.clear();
            }
        }
        let p = self.primary;
        self.set_sels(new, p);
        self.expand_stack.push((before, self.sels.clone()));
    }

    /// IntelliJ "Shrink Selection" (Ctrl+Shift+W).
    pub fn shrink_selection(&mut self) {
        if let Some((before, after)) = self.expand_stack.pop() {
            if same_ranges(&after, &self.sels) {
                let p = self.primary.min(before.len() - 1);
                self.set_sels(before, p);
                return;
            }
            self.expand_stack.clear();
        }
        // No history: shrink to the camel hump / caret.
        let p = self.primary_sel();
        if !p.is_empty() {
            let h = p.head;
            self.set_sels(vec![Sel::caret(h)], 0);
        }
    }

    fn occurrences_of_primary(&self, whole: bool) -> Vec<(usize, usize)> {
        let p = self.primary_sel();
        let needle = self.rope.slice(p.start()..p.end()).to_string();
        let hay = self.rope.to_string();
        search::find_all(&self.rope, &hay, &needle, true, whole, false)
    }

    /// Sublime Ctrl+D / IntelliJ Alt+J.
    pub fn select_next_occurrence(&mut self) {
        if self.primary_sel().is_empty() {
            let rope = &self.rope;
            let new: Vec<Sel> = self
                .sels
                .iter()
                .map(|s| word_at(rope, s.head).map(|r| Sel::new(r.start, r.end)).unwrap_or(*s))
                .collect();
            let p = self.primary;
            self.set_sels(new, p);
            self.occ_whole_word = true;
            return;
        }
        let whole = self.occ_whole_word;
        let p = self.primary_sel();
        let matches = self.occurrences_of_primary(whole);
        let selected = |m: &(usize, usize)| self.sels.iter().any(|s| s.start() == m.0 && s.end() == m.1);
        let next = matches
            .iter()
            .find(|m| m.0 >= p.end() && !selected(m))
            .or_else(|| matches.iter().find(|m| !selected(m)))
            .copied();
        if let Some((a, b)) = next {
            let mut sels = self.sels.clone();
            sels.push(Sel::new(a, b));
            let idx = sels.len() - 1;
            self.set_sels(sels, idx);
            self.occ_whole_word = whole;
        }
    }

    /// IntelliJ Shift+Alt+J: drop the most recently added occurrence.
    pub fn unselect_occurrence(&mut self) {
        if self.sels.len() > 1 {
            let whole = self.occ_whole_word;
            let mut sels = self.sels.clone();
            sels.remove(self.primary);
            let p = self.primary.saturating_sub(1);
            self.set_sels(sels, p);
            self.occ_whole_word = whole;
        }
    }

    pub fn select_all_occurrences(&mut self) {
        let mut whole = self.occ_whole_word;
        if self.primary_sel().is_empty() {
            let p = self.primary_sel();
            match word_at(&self.rope, p.head) {
                Some(r) => {
                    self.set_sels(vec![Sel::new(r.start, r.end)], 0);
                    whole = true;
                }
                None => return,
            }
        }
        let matches = self.occurrences_of_primary(whole);
        if matches.is_empty() {
            return;
        }
        let p = self.primary_sel();
        let prim = matches.iter().position(|m| m.0 == p.start()).unwrap_or(0);
        self.set_sels(matches.into_iter().map(|(a, b)| Sel::new(a, b)).collect(), prim);
    }

    /// Ctrl+Shift+L: split multi-line selections into one selection per line.
    pub fn split_into_lines(&mut self) {
        let rope = &self.rope;
        let mut out = vec![];
        for s in &self.sels {
            let (st, en) = (s.start(), s.end());
            let l0 = rope.char_to_line(st);
            let l1 = rope.char_to_line(en);
            if l0 == l1 {
                out.push(*s);
                continue;
            }
            for l in l0..=l1 {
                let a = if l == l0 { st } else { rope.line_to_char(l) };
                let b = if l == l1 { en } else { line_end(rope, l) };
                if l == l1 && b == rope.line_to_char(l) {
                    continue;
                }
                out.push(Sel::new(a, b));
            }
        }
        let n = out.len();
        self.set_sels(out, n.saturating_sub(1));
    }

    pub fn add_caret(&mut self, up: bool) {
        let rope = &self.rope;
        let tab = self.tab_size;
        let s = if up { self.sels[0] } else { *self.sels.last().unwrap() };
        let (l, c) = pos_to_lc(rope, s.head);
        let goal = s.goal.unwrap_or_else(|| visual_col(rope, l, c, tab));
        let target = if up {
            if l == 0 {
                return;
            }
            l - 1
        } else {
            if l + 1 >= rope.len_lines() {
                return;
            }
            l + 1
        };
        let p = rope.line_to_char(target) + col_from_visual(rope, target, goal, tab);
        let mut sels = self.sels.clone();
        sels.push(Sel { anchor: p, head: p, goal: Some(goal) });
        let idx = sels.len() - 1;
        self.set_sels(sels, idx);
    }

    pub fn add_caret_at(&mut self, pos: usize) {
        let mut sels = self.sels.clone();
        // Ctrl+click on an existing caret removes it.
        if sels.len() > 1 {
            if let Some(i) = sels.iter().position(|s| s.is_empty() && s.head == pos) {
                sels.remove(i);
                let n = sels.len();
                return self.set_sels(sels, n - 1);
            }
        }
        sels.push(Sel::caret(pos));
        let idx = sels.len() - 1;
        self.set_sels(sels, idx);
    }

    pub fn goto_matching_bracket(&mut self) {
        let rope = &self.rope;
        let new: Vec<Sel> = self
            .sels
            .iter()
            .map(|s| match matching_bracket(rope, s.head, 5_000_000) {
                Some((_, m)) => Sel::caret(m),
                None => match enclosing_brackets(rope, s.head, s.head) {
                    Some((_, b)) => Sel::caret(b),
                    None => *s,
                },
            })
            .collect();
        let p = self.primary;
        self.set_sels(new, p);
    }

    pub fn goto_line(&mut self, line: usize) {
        let l = line.saturating_sub(1).min(self.rope.len_lines() - 1);
        let p = self.rope.line_to_char(l) + first_non_ws(&self.rope, l);
        self.set_sels(vec![Sel::caret(p)], 0);
    }

    // ------------------------------------------------------------ line operations

    fn sel_line_range(&self, s: &Sel) -> (usize, usize) {
        let l0 = self.rope.char_to_line(s.start());
        let mut l1 = self.rope.char_to_line(s.end());
        if l1 > l0 && s.end() == self.rope.line_to_char(l1) {
            l1 -= 1;
        }
        (l0, l1)
    }

    fn line_blocks(&self) -> Vec<(usize, usize)> {
        let mut v: Vec<(usize, usize)> = self.sels.iter().map(|s| self.sel_line_range(s)).collect();
        v.sort();
        let mut out: Vec<(usize, usize)> = vec![];
        for (a, b) in v {
            if let Some(last) = out.last_mut() {
                if a <= last.1 + 1 {
                    last.1 = last.1.max(b);
                    continue;
                }
            }
            out.push((a, b));
        }
        out
    }

    /// Generic line-block transform. `ext` may grow each block (returning None aborts),
    /// `f` maps the block's lines to new lines plus a (line_off, col) -> (line_off, col) map
    /// for selections inside the block.
    fn line_op(
        &mut self,
        ext: impl Fn(usize, usize, usize) -> Option<(usize, usize)>,
        mut f: impl FnMut(&[String]) -> (Vec<String>, LineMap),
    ) -> bool {
        let nlines = self.rope.len_lines();
        let mut blocks = Vec::new();
        for (a, b) in self.line_blocks() {
            match ext(a, b, nlines) {
                Some(x) => blocks.push(x),
                None => return false,
            }
        }
        blocks.sort();
        let mut merged: Vec<(usize, usize)> = vec![];
        for (a, b) in blocks {
            if let Some(l) = merged.last_mut() {
                if a <= l.1 {
                    l.1 = l.1.max(b);
                    continue;
                }
            }
            merged.push((a, b));
        }
        let results: Vec<(usize, usize, Vec<String>, LineMap)> = merged
            .iter()
            .map(|&(a, b)| {
                let lines: Vec<String> = (a..=b).map(|l| line_text(&self.rope, l)).collect();
                let (nl, m) = f(&lines);
                (a, b, nl, m)
            })
            .collect();
        let mut starts = vec![];
        let mut shift: isize = 0;
        for (a, b, nl, _) in &results {
            starts.push((*a as isize + shift) as usize);
            shift += nl.len() as isize - (b - a + 1) as isize;
        }
        let map = |pos: usize| -> (usize, usize) {
            let (l, c) = pos_to_lc(&self.rope, pos);
            let mut sh: isize = 0;
            for (i, (a, b, nl, m)) in results.iter().enumerate() {
                if l >= *a && l <= *b {
                    let (nl2, nc) = m(l - a, c);
                    return (starts[i] + nl2, nc);
                }
                if *b < l {
                    sh += nl.len() as isize - (b - a + 1) as isize;
                }
            }
            ((l as isize + sh).max(0) as usize, c)
        };
        let new_lc: Vec<((usize, usize), (usize, usize))> =
            self.sels.iter().map(|s| (map(s.anchor), map(s.head))).collect();
        self.checkpoint(EditKind::Other);
        let first = results.first().map(|r| r.0).unwrap_or(0);
        for (a, b, nl, _) in results.iter().rev() {
            let s0 = self.rope.line_to_char(*a);
            let (mut s, e, trail) = if b + 1 < self.rope.len_lines() {
                (s0, self.rope.line_to_char(b + 1), true)
            } else {
                (s0, self.rope.len_chars(), false)
            };
            let mut t = nl.join("\n");
            if nl.is_empty() {
                if !trail && s > 0 {
                    s -= 1;
                }
            } else if trail {
                t.push('\n');
            }
            self.rope.remove(s..e);
            self.rope.insert(s, &t);
        }
        let sels = new_lc
            .into_iter()
            .map(|((al, ac), (hl, hc))| Sel::new(lc_to_pos(&self.rope, al, ac), lc_to_pos(&self.rope, hl, hc)))
            .collect();
        self.sels = sels;
        let len = self.rope.len_chars();
        normalize(&mut self.sels, &mut self.primary, len);
        self.bump(first);
        true
    }

    pub fn move_lines(&mut self, up: bool) {
        if up {
            self.line_op(
                |a, b, _| if a == 0 { None } else { Some((a - 1, b)) },
                |lines| {
                    let n = lines.len();
                    let mut v = lines[1..].to_vec();
                    v.push(lines[0].clone());
                    (v, lm(move |k, c| if k == 0 { (n - 1, c) } else { (k - 1, c) }))
                },
            );
        } else {
            let n_lines = self.rope.len_lines();
            let last_empty = line_len(&self.rope, n_lines - 1) == 0;
            let limit = if last_empty { n_lines.saturating_sub(2) } else { n_lines - 1 };
            self.line_op(
                |a, b, _| if b >= limit { None } else { Some((a, b + 1)) },
                |lines| {
                    let n = lines.len();
                    let mut v = vec![lines[n - 1].clone()];
                    v.extend_from_slice(&lines[..n - 1]);
                    (v, lm(move |k, c| if k == n - 1 { (0, c) } else { (k + 1, c) }))
                },
            );
        }
    }

    /// Duplicate selection, or the line(s) if nothing is selected.
    pub fn duplicate(&mut self) {
        if self.sels.iter().any(|s| !s.is_empty()) {
            let rope = &self.rope;
            let edits: Vec<Edit> = self
                .sels
                .iter()
                .map(|s| {
                    let t = rope.slice(s.start()..s.end()).to_string();
                    let n = t.chars().count();
                    let (a, h) = if s.reversed() { (n, 0) } else { (0, n) };
                    Edit { start: s.end(), end: s.end(), text: t, anchor: a, head: h }
                })
                .collect();
            self.apply(EditKind::Other, edits);
        } else {
            self.line_op(
                |a, b, _| Some((a, b)),
                |lines| {
                    let n = lines.len();
                    let mut v = lines.to_vec();
                    v.extend_from_slice(lines);
                    (v, lm(move |k, c| (k + n, c)))
                },
            );
        }
    }

    pub fn delete_lines(&mut self) {
        self.line_op(|a, b, _| Some((a, b)), |_| (vec![], lm(|_, c| (0, c))));
    }

    pub fn indent(&mut self) {
        let unit = self.indent_unit();
        let u = unit.chars().count();
        self.line_op(
            |a, b, _| Some((a, b)),
            |lines| {
                let empty: Vec<bool> = lines.iter().map(|l| l.trim().is_empty()).collect();
                let v = lines
                    .iter()
                    .zip(&empty)
                    .map(|(l, e)| if *e { l.clone() } else { format!("{unit}{l}") })
                    .collect();
                (v, lm(move |k, c| if empty[k] { (k, c) } else { (k, c + u) }))
            },
        );
    }

    pub fn outdent(&mut self) {
        let tab = self.tab_size;
        self.line_op(
            |a, b, _| Some((a, b)),
            |lines| {
                let mut rem = vec![];
                let mut v = vec![];
                for l in lines {
                    let r = if l.starts_with('\t') {
                        1
                    } else {
                        l.chars().take(tab).take_while(|c| *c == ' ').count()
                    };
                    rem.push(r);
                    v.push(l.chars().skip(r).collect::<String>());
                }
                (v, lm(move |k, c| (k, c.saturating_sub(rem[k]))))
            },
        );
    }

    pub fn toggle_comment(&mut self) {
        let tok = self.comment_token().to_string();
        let tn = tok.chars().count();
        self.line_op(
            |a, b, _| Some((a, b)),
            |lines| {
                let nonblank: Vec<bool> = lines.iter().map(|l| !l.trim().is_empty()).collect();
                if !nonblank.iter().any(|x| *x) {
                    return (lines.to_vec(), lm(|k, c| (k, c)));
                }
                let indent_of = |l: &str| l.chars().take_while(|c| *c == ' ' || *c == '\t').count();
                let all = lines
                    .iter()
                    .zip(&nonblank)
                    .all(|(l, nb)| !nb || l.trim_start().starts_with(tok.as_str()));
                if all {
                    let mut out = vec![];
                    let mut info = vec![];
                    for (l, nb) in lines.iter().zip(&nonblank) {
                        if !nb {
                            out.push(l.clone());
                            info.push((0, 0));
                            continue;
                        }
                        let ind = indent_of(l);
                        let chars: Vec<char> = l.chars().collect();
                        let mut r = tn;
                        if chars.get(ind + r) == Some(&' ') {
                            r += 1;
                        }
                        out.push(chars[..ind].iter().chain(chars[ind + r..].iter()).collect());
                        info.push((ind, r));
                    }
                    (
                        out,
                        lm(move |k, c| {
                            let (ind, r) = info[k];
                            if c > ind { (k, c.saturating_sub(r).max(ind)) } else { (k, c) }
                        }),
                    )
                } else {
                    let min_ind = lines
                        .iter()
                        .zip(&nonblank)
                        .filter(|(_, nb)| **nb)
                        .map(|(l, _)| indent_of(l))
                        .min()
                        .unwrap_or(0);
                    let ins = format!("{tok} ");
                    let n = ins.chars().count();
                    let out = lines
                        .iter()
                        .zip(&nonblank)
                        .map(|(l, nb)| {
                            if !nb {
                                return l.clone();
                            }
                            let chars: Vec<char> = l.chars().collect();
                            let mut s: String = chars[..min_ind].iter().collect();
                            s.push_str(&ins);
                            s.extend(chars[min_ind..].iter());
                            s
                        })
                        .collect();
                    let nb = nonblank.clone();
                    (out, lm(move |k, c| if nb[k] && c >= min_ind { (k, c + n) } else { (k, c) }))
                }
            },
        );
    }

    pub fn join_lines(&mut self) {
        self.line_op(
            |a, b, n| Some((a, if a == b { (b + 1).min(n - 1) } else { b })),
            |lines| {
                let mut out = lines[0].trim_end().to_string();
                let mut info = vec![(0usize, 0usize)];
                for l in &lines[1..] {
                    let lead = l.chars().take_while(|c| c.is_whitespace()).count();
                    let t = l.trim();
                    if !t.is_empty() && !out.is_empty() {
                        out.push(' ');
                    }
                    info.push((out.chars().count(), lead));
                    out.push_str(t);
                }
                (
                    vec![out],
                    lm(move |k, c| {
                        let (off, lead) = info[k];
                        (0, if k == 0 { c } else { off + c.saturating_sub(lead) })
                    }),
                )
            },
        );
    }

    pub fn sort_lines(&mut self) {
        self.line_op(
            |a, b, _| Some((a, b)),
            |lines| {
                let mut v = lines.to_vec();
                v.sort_by_key(|s| s.to_lowercase());
                (v, lm(|k, c| (k, c)))
            },
        );
    }
}

fn motion_target(rope: &Rope, s: &Sel, m: Motion, tab: usize, camel: bool) -> (usize, Option<usize>) {
    let len = rope.len_chars();
    let h = s.head;
    match m {
        Motion::Left => (h.saturating_sub(1), None),
        Motion::Right => ((h + 1).min(len), None),
        Motion::WordLeft => (prev_word(rope, h, camel), None),
        Motion::WordRight => (next_word(rope, h, camel), None),
        Motion::Up | Motion::Down | Motion::PageUp(_) | Motion::PageDown(_) => {
            let (l, c) = pos_to_lc(rope, h);
            let goal = s.goal.unwrap_or_else(|| visual_col(rope, l, c, tab));
            let last = rope.len_lines() - 1;
            let target = match m {
                Motion::Up if l == 0 => return (0, None),
                Motion::Down if l == last => return (len, None),
                Motion::Up => l - 1,
                Motion::Down => l + 1,
                Motion::PageUp(n) => l.saturating_sub(n),
                Motion::PageDown(n) => (l + n).min(last),
                _ => unreachable!(),
            };
            (rope.line_to_char(target) + col_from_visual(rope, target, goal, tab), Some(goal))
        }
        Motion::Home => {
            let (l, c) = pos_to_lc(rope, h);
            let fnw = first_non_ws(rope, l);
            (rope.line_to_char(l) + if c == fnw { 0 } else { fnw }, None)
        }
        Motion::End => (line_end(rope, rope.char_to_line(h)), None),
        Motion::DocStart => (0, None),
        Motion::DocEnd => (len, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str, sels: &[(usize, usize)]) -> Document {
        let mut d = Document::from_str(text);
        d.set_sels(sels.iter().map(|&(a, h)| Sel::new(a, h)).collect(), 0);
        d
    }
    fn ranges(d: &Document) -> Vec<(usize, usize)> {
        d.sels.iter().map(|s| (s.anchor, s.head)).collect()
    }

    #[test]
    fn multi_caret_typing_and_undo() {
        let mut d = doc("ab\ncd\n", &[(1, 1), (4, 4)]);
        d.type_text("X");
        d.type_text("Y");
        assert_eq!(d.rope.to_string(), "aXYb\ncXYd\n");
        assert_eq!(ranges(&d), vec![(3, 3), (8, 8)]);
        d.undo();
        assert_eq!(d.rope.to_string(), "ab\ncd\n");
        assert!(!d.is_dirty());
        d.redo();
        assert_eq!(d.rope.to_string(), "aXYb\ncXYd\n");
    }

    #[test]
    fn auto_pairs() {
        let mut d = doc("", &[(0, 0)]);
        d.type_text("(");
        assert_eq!(d.rope.to_string(), "()");
        d.type_text(")");
        assert_eq!(d.rope.to_string(), "()");
        assert_eq!(ranges(&d), vec![(2, 2)]);
        let mut d = doc("abc", &[(0, 3)]);
        d.type_text("\"");
        assert_eq!(d.rope.to_string(), "\"abc\"");
        let mut d = doc("don", &[(3, 3)]);
        d.type_text("'");
        assert_eq!(d.rope.to_string(), "don'");
    }

    #[test]
    fn newline_indents_between_braces() {
        let mut d = doc("    f{}", &[(6, 6)]);
        d.newline();
        assert_eq!(d.rope.to_string(), "    f{\n        \n    }");
        assert_eq!(ranges(&d), vec![(15, 15)]);
    }

    #[test]
    fn move_and_duplicate_lines() {
        let mut d = doc("a\nb\nc\n", &[(2, 2)]);
        d.move_lines(true);
        assert_eq!(d.rope.to_string(), "b\na\nc\n");
        assert_eq!(ranges(&d), vec![(0, 0)]);
        d.move_lines(false);
        d.move_lines(false);
        assert_eq!(d.rope.to_string(), "a\nc\nb\n");
        d.duplicate();
        assert_eq!(d.rope.to_string(), "a\nc\nb\nb\n");
        assert_eq!(ranges(&d), vec![(6, 6)]);
    }

    #[test]
    fn comment_toggle() {
        let mut d = doc("  a\n  b\n", &[(0, 6)]);
        d.path = Some("x.rs".into());
        d.toggle_comment();
        assert_eq!(d.rope.to_string(), "  // a\n  // b\n");
        d.toggle_comment();
        assert_eq!(d.rope.to_string(), "  a\n  b\n");
    }

    #[test]
    fn join_and_delete() {
        let mut d = doc("a\n   b\nc", &[(0, 0)]);
        d.join_lines();
        assert_eq!(d.rope.to_string(), "a b\nc");
        d.delete_lines();
        assert_eq!(d.rope.to_string(), "c");
    }

    #[test]
    fn next_occurrence() {
        let mut d = doc("foo bar foo foobar foo", &[(1, 1)]);
        d.select_next_occurrence();
        assert_eq!(ranges(&d), vec![(0, 3)]);
        d.select_next_occurrence();
        d.select_next_occurrence();
        // whole-word: skips "foobar"
        assert_eq!(ranges(&d), vec![(0, 3), (8, 11), (19, 22)]);
        d.type_text("x");
        assert_eq!(d.rope.to_string(), "x bar x foobar x");
    }

    #[test]
    fn expand_and_shrink() {
        let mut d = doc("call(\"myHttpClient\", 2)", &[(10, 10)]);
        d.expand_selection();
        assert_eq!(ranges(&d), vec![(8, 12)]); // Http
        d.expand_selection();
        assert_eq!(ranges(&d), vec![(6, 18)]); // myHttpClient
        d.expand_selection();
        assert_eq!(ranges(&d), vec![(5, 19)]); // "myHttpClient"
        d.expand_selection();
        assert_eq!(ranges(&d), vec![(5, 22)]); // args
        d.shrink_selection();
        d.shrink_selection();
        assert_eq!(ranges(&d), vec![(6, 18)]);
    }

    #[test]
    fn column_block_typing_pads_short_lines() {
        let mut d = doc("abcdef\nab\nabcdef\n", &[(0, 0)]);
        d.set_block(Block { anchor_line: 0, anchor_col: 4, head_line: 2, head_col: 4 });
        d.block_replace(&["X".into()], EditKind::Insert);
        assert_eq!(d.rope.to_string(), "abcdXef\nab  X\nabcdXef\n");
        assert_eq!(d.block.unwrap().cols(), (5, 5));
        d.block_backspace();
        assert_eq!(d.rope.to_string(), "abcdef\nab  \nabcdef\n");
        d.undo(); // undo backspace
        assert_eq!(d.rope.to_string(), "abcdXef\nab  X\nabcdXef\n");
        d.undo(); // undo typing, including the padding
        assert_eq!(d.rope.to_string(), "abcdef\nab\nabcdef\n");
    }

    #[test]
    fn column_block_copy_and_per_line_paste() {
        let mut d = doc("abcdef\nab\nabcdef", &[(0, 0)]);
        d.set_block(Block { anchor_line: 0, anchor_col: 1, head_line: 2, head_col: 4 });
        assert_eq!(d.block_copy(), "bcd\nb  \nbcd");
        d.block_replace(&["1".into(), "2".into(), "3".into()], EditKind::Other);
        assert_eq!(d.rope.to_string(), "a1ef\na2\na3ef");
    }

    #[test]
    fn line_end_carets_and_block_move() {
        let mut d = doc("ab\nabcd\nx", &[(0, 6)]);
        d.carets_at_line_ends();
        assert_eq!(ranges(&d), vec![(2, 2), (7, 7)]);
        let mut d = doc("abc\nabc\nabc", &[(1, 1)]);
        d.toggle_column_mode();
        d.block_move(1, 0, true);
        d.block_move(0, 1, true);
        assert_eq!(ranges(&d), vec![(1, 2), (5, 6)]);
        d.block_move(0, 5, true); // into virtual space past the line end
        assert_eq!(d.block.unwrap().head_col, 7);
        d.block_replace(&["Z".into()], EditKind::Insert);
        assert_eq!(d.rope.to_string(), "aZ\naZ\nabc");
    }

    #[test]
    fn rectangle_selection_clamps_short_lines() {
        let r = Rope::from_str("abcdef\nab\nabcdef");
        let (s, p) = rect_sels(&r, 4, 0, 1, 2, 4);
        let v: Vec<(usize, usize)> = s.iter().map(|s| (s.anchor, s.head)).collect();
        assert_eq!(v, vec![(1, 4), (8, 9), (11, 14)]);
        assert_eq!(p, 2);
    }

    #[test]
    fn line_copy_paste() {
        let mut d = doc("one\ntwo\n", &[(5, 5)]);
        let (t, line) = d.copy();
        assert_eq!((t.as_str(), line), ("two\n", true));
        d.set_sels(vec![Sel::caret(1)], 0);
        d.paste(&t, true);
        assert_eq!(d.rope.to_string(), "two\none\ntwo\n");
    }
}
