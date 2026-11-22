//! Application shell: tabs, sidebar, command palette, find bar, dialogs.

use crate::doc::{Document, EditKind, Motion};
use crate::editor::{self, Settings, View};
use crate::highlight::{config_dir, lerp, Colors, LineCache, Syntax};
use crate::keymap::{self, Cmd};
use crate::search;
use crate::selection::Sel;
use crate::session::{FileState, Session};
use crate::text::line_text;
use eframe::egui::{
    self, text::LayoutJob, vec2, Align2, Color32, Event, FontData, FontDefinitions, FontFamily,
    FontId, Frame, Id, ImeEvent, Key, Margin, Modifiers, RichText, Sense, TextEdit, TextFormat,
    ViewportCommand,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Tab {
    pub doc: Document,
    pub view: View,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PMode {
    Commands,
    Files,
    Themes,
    Syntaxes,
    Clipboard,
}

#[derive(Clone)]
enum PAction {
    Cmd(Cmd),
    Open(PathBuf),
    Line(usize),
    Theme(String),
    Syntax(String),
    Clip(usize),
}

struct PItem {
    label: String,
    detail: String,
    hits: Vec<usize>,
    action: PAction,
}

struct Palette {
    mode: PMode,
    query: String,
    selected: usize,
    focus: bool,
    cache: Option<(String, Vec<PItem>)>,
}

#[derive(Default)]
struct Find {
    open: bool,
    replace: bool,
    query: String,
    replacement: String,
    case: bool,
    whole: bool,
    regex: bool,
    matches: Vec<(usize, usize)>,
    key: Option<(u64, u64, String, bool, bool, bool)>,
    focus: bool,
    origin: usize,
}

enum Pending {
    CloseTab(usize),
    Quit,
}

enum Action {
    Cmd(Cmd),
    Text(String),
    Paste(String),
    Copy,
    Cut,
}

pub struct App {
    tabs: Vec<Tab>,
    active: usize,
    syn: Syntax,
    theme_name: String,
    colors: Colors,
    settings: Settings,
    default_font: f32,
    sidebar: bool,
    root: Option<PathBuf>,
    dir_cache: HashMap<PathBuf, Vec<(PathBuf, bool)>>,
    files: Vec<PathBuf>,
    palette: Option<Palette>,
    find: Find,
    clip_history: Vec<String>,
    line_clip: Option<String>,
    pending: Option<Pending>,
    allow_quit: bool,
    title: String,
    editor_id: Id,
    status: Option<(String, f64)>,
    camel_humps: bool,
    to_open: Option<PathBuf>,
    /// Last session text written to disk, and when.
    session_text: String,
    session_saved_at: f64,
    /// Focus the editor once it exists (egui drops focus requests for unseen widgets).
    startup_focus: bool,
    ctrl_prev: bool,
    ctrl_last_press: f64,
    double_ctrl: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext, args: Vec<PathBuf>) -> Self {
        setup_fonts(&cc.egui_ctx);
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let syn = Syntax::load();
        let theme_name = ["base16-ocean.dark", "base16-eighties.dark"]
            .into_iter()
            .find(|t| syn.themes.themes.contains_key(*t))
            .map(String::from)
            .unwrap_or_else(|| syn.themes.themes.keys().next().cloned().unwrap_or_default());
        let colors = Colors::from_theme(syn.theme(&theme_name));
        let plain = syn.ss.find_syntax_plain_text().name.clone();
        let mut app = Self {
            tabs: vec![Tab { doc: Document::new(), view: View::new(plain) }],
            active: 0,
            syn,
            theme_name,
            colors,
            settings: Settings { font_size: 14.0, line_spacing: 1.35, minimap: true },
            default_font: 14.0,
            sidebar: true,
            root: None,
            dir_cache: HashMap::new(),
            files: vec![],
            palette: None,
            find: Find { case: false, ..Default::default() },
            clip_history: vec![],
            line_clip: None,
            pending: None,
            allow_quit: false,
            title: String::new(),
            editor_id: Id::new("editor"),
            status: None,
            camel_humps: true,
            to_open: None,
            session_text: String::new(),
            session_saved_at: 0.0,
            startup_focus: false,
            ctrl_prev: false,
            ctrl_last_press: -1.0,
            double_ctrl: false,
        };
        let session = Session::load();
        let had_session = session.is_some();
        if let Some(s) = session {
            app.restore(s);
        }
        app.apply_visuals(&cc.egui_ctx);
        for a in args {
            app.open_path(&a);
        }
        if !had_session && app.root.is_none() && app.tabs.iter().all(|t| t.doc.path.is_none()) {
            app.sidebar = false;
        }
        app.startup_focus = true;
        app
    }

    // ------------------------------------------------------------ session

    fn restore(&mut self, s: Session) {
        if let Some(t) = s.theme.filter(|t| self.syn.themes.themes.contains_key(t)) {
            self.theme_name = t;
        }
        if let Some(f) = s.font_size {
            self.settings.font_size = f.clamp(6.0, 48.0);
        }
        if let Some(m) = s.minimap {
            self.settings.minimap = m;
        }
        if let Some(c) = s.camel_humps {
            self.camel_humps = c;
        }
        if let Some(r) = s.root.filter(|r| r.is_dir()) {
            self.set_root(r);
        }
        for f in &s.files {
            if !f.path.is_file() {
                continue; // deleted or moved since last run
            }
            self.open_path(&f.path);
            let t = self.cur();
            if t.doc.path.as_deref() == Some(f.path.as_path()) {
                t.doc.set_sels(vec![Sel::new(f.anchor, f.head)], 0);
                t.view.scroll.y = f.scroll_y;
            }
        }
        if let Some(p) = s.files.get(s.active).map(|f| &f.path) {
            if let Some(i) = self.tabs.iter().position(|t| t.doc.path.as_deref() == Some(p.as_path())) {
                self.active = i;
            }
        }
        if let Some(b) = s.sidebar {
            self.sidebar = b;
        }
    }

    fn session(&self) -> Session {
        let with_path = |t: &Tab| t.doc.path.is_some();
        Session {
            root: self.root.clone(),
            files: self
                .tabs
                .iter()
                .filter_map(|t| {
                    let s = t.doc.primary_sel();
                    Some(FileState { path: t.doc.path.clone()?, anchor: s.anchor, head: s.head, scroll_y: t.view.scroll.y })
                })
                .collect(),
            active: self.tabs[..self.active].iter().filter(|t| with_path(t)).count(),
            theme: Some(self.theme_name.clone()),
            font_size: Some(self.settings.font_size),
            sidebar: Some(self.sidebar),
            minimap: Some(self.settings.minimap),
            camel_humps: Some(self.camel_humps),
        }
    }

    /// Save the session when it changed: throttled to every 2s, immediately when `force`.
    fn persist_session(&mut self, ctx: &egui::Context, force: bool) {
        let text = self.session().to_text();
        if text == self.session_text {
            return;
        }
        let now = ctx.input(|i| i.time);
        let since = now - self.session_saved_at;
        if !force && since < 2.0 {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(2.05 - since));
            return;
        }
        if let Err(e) = Session::save_text(&text) {
            self.set_status(ctx, format!("Could not save session: {e}"));
        }
        self.session_text = text;
        self.session_saved_at = now;
    }

    fn apply_visuals(&mut self, ctx: &egui::Context) {
        self.colors = Colors::from_theme(self.syn.theme(&self.theme_name));
        let c = &self.colors;
        let mut v = if c.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        v.panel_fill = c.panel;
        v.window_fill = lerp(c.panel, c.bg, 0.5);
        v.extreme_bg_color = c.bg;
        v.selection.bg_fill = c.selection;
        v.widgets.noninteractive.fg_stroke.color = lerp(c.panel, c.fg, 0.75);
        ctx.set_visuals(v);
    }

    fn cur(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    fn set_status(&mut self, ctx: &egui::Context, msg: impl Into<String>) {
        self.status = Some((msg.into(), ctx.input(|i| i.time)));
    }

    fn focus_editor(&self, ctx: &egui::Context) {
        ctx.memory_mut(|m| m.request_focus(self.editor_id));
    }

    // ------------------------------------------------------------ files

    fn new_tab(&mut self) {
        let plain = self.syn.ss.find_syntax_plain_text().name.clone();
        let mut doc = Document::new();
        doc.camel_humps = self.camel_humps;
        self.tabs.push(Tab { doc, view: View::new(plain) });
        self.active = self.tabs.len() - 1;
    }

    fn open_path(&mut self, p: &Path) {
        let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        let p = abs.as_path();
        if p.is_dir() {
            self.set_root(p.to_path_buf());
            return;
        }
        if let Some(i) = self.tabs.iter().position(|t| t.doc.path.as_deref() == Some(p)) {
            self.active = i;
            return;
        }
        match Document::open(p) {
            Ok(mut doc) => {
                doc.camel_humps = self.camel_humps;
                let first = if doc.rope.len_lines() > 0 { line_text(&doc.rope, 0) } else { String::new() };
                let syntax = self.syn.detect(Some(p), &first);
                let tab = Tab { doc, view: View::new(syntax) };
                if self.tabs.len() == 1 && self.tabs[0].doc.is_pristine() {
                    self.tabs[0] = tab;
                    self.active = 0;
                } else {
                    self.tabs.push(tab);
                    self.active = self.tabs.len() - 1;
                }
            }
            Err(e) => self.status = Some((format!("Cannot open {}: {e}", p.display()), 0.0)),
        }
    }

    fn set_root(&mut self, dir: PathBuf) {
        let dir = std::path::absolute(&dir).unwrap_or(dir);
        self.dir_cache.clear();
        self.files = walk(&dir, 50_000);
        self.root = Some(dir);
        self.sidebar = true;
    }

    fn save(&mut self, i: usize) -> bool {
        let Some(p) = self.tabs[i].doc.path.clone() else {
            return self.save_as(i);
        };
        match self.tabs[i].doc.save_to(&p) {
            Ok(()) => true,
            Err(e) => {
                self.status = Some((format!("Save failed: {e}"), 0.0));
                false
            }
        }
    }

    fn save_as(&mut self, i: usize) -> bool {
        let mut dlg = rfd::FileDialog::new().set_file_name(self.tabs[i].doc.name());
        if let Some(dir) = self.tabs[i].doc.path.as_ref().and_then(|p| p.parent()).or(self.root.as_deref()) {
            dlg = dlg.set_directory(dir);
        }
        let Some(p) = dlg.save_file() else { return false };
        match self.tabs[i].doc.save_to(&p) {
            Ok(()) => {
                let first = line_text(&self.tabs[i].doc.rope, 0);
                let syntax = self.syn.detect(Some(&p), &first);
                self.tabs[i].view.cache = LineCache::new(syntax);
                true
            }
            Err(e) => {
                self.status = Some((format!("Save failed: {e}"), 0.0));
                false
            }
        }
    }

    fn close_tab(&mut self, i: usize) {
        if self.tabs[i].doc.is_dirty() {
            self.pending = Some(Pending::CloseTab(i));
            return;
        }
        self.force_close(i);
    }

    fn force_close(&mut self, i: usize) {
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            self.new_tab();
        }
        if self.active >= i && self.active > 0 {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len() - 1);
    }

    // ------------------------------------------------------------ commands

    fn store_clip(&mut self, ctx: &egui::Context, text: String, line: bool) {
        if text.is_empty() {
            return;
        }
        ctx.output_mut(|o| o.copied_text = text.clone());
        self.line_clip = line.then(|| text.clone());
        self.clip_history.retain(|h| h != &text);
        self.clip_history.insert(0, text);
        self.clip_history.truncate(30);
    }

    /// Column Selection Mode: route edits to the rectangular block. Returns true if handled.
    fn block_action(&mut self, ctx: &egui::Context, a: &Action) -> bool {
        let doc = &mut self.tabs[self.active].doc;
        if doc.block.is_none() {
            // In column mode, Shift+arrows start a block at the caret.
            let starts = doc.column_mode
                && doc.sels.len() == 1
                && matches!(a, Action::Cmd(Cmd::Move(Motion::Left | Motion::Right | Motion::Up | Motion::Down, true)));
            if !starts {
                return false;
            }
            let b = doc.block_from_primary();
            doc.set_block(b);
        }
        let b = doc.block.unwrap();
        let (l0, l1) = b.lines();
        match a {
            Action::Text(t) if !t.contains('\n') => doc.block_replace(&[t.clone()], EditKind::Insert),
            Action::Paste(t) => {
                let t = t.replace("\r\n", "\n");
                let parts: Vec<String> = t.strip_suffix('\n').unwrap_or(&t).split('\n').map(String::from).collect();
                if parts.len() != 1 && parts.len() != l1 - l0 + 1 {
                    doc.block = None;
                    return false;
                }
                doc.block_replace(&parts, EditKind::Other);
            }
            Action::Copy | Action::Cut => {
                let text = doc.block_copy();
                if matches!(a, Action::Cut) {
                    doc.block_replace(&[String::new()], EditKind::Delete);
                }
                self.store_clip(ctx, text, false);
            }
            Action::Cmd(Cmd::Backspace(_)) => doc.block_backspace(),
            Action::Cmd(Cmd::DeleteFwd(_)) => doc.block_delete(),
            Action::Cmd(Cmd::Tab) => {
                let n = doc.tab_size - b.cols().0 % doc.tab_size;
                doc.block_replace(&[" ".repeat(n)], EditKind::Insert);
            }
            Action::Cmd(Cmd::Move(m, ext)) => {
                let (dl, dc) = match m {
                    Motion::Left => (0, -1),
                    Motion::Right => (0, 1),
                    Motion::Up => (-1, 0),
                    Motion::Down => (1, 0),
                    _ => {
                        doc.block = None;
                        return false;
                    }
                };
                doc.block_move(dl, dc, *ext);
            }
            Action::Cmd(Cmd::Escape) => {
                let h = doc.primary_sel().head;
                doc.set_sels(vec![Sel::caret(h)], 0);
            }
            Action::Cmd(c) if c.is_global() => return false,
            _ => {
                // Any other command works on the (clamped) per-line selections.
                doc.block = None;
                return false;
            }
        }
        self.cur().view.scroll_to_caret = true;
        true
    }

    fn run(&mut self, ctx: &egui::Context, a: Action) {
        if self.block_action(ctx, &a) {
            return;
        }
        let page = self.tabs[self.active].view.page_lines;
        match a {
            Action::Text(t) => self.cur().doc.type_text(&t),
            Action::Paste(t) => {
                let norm = t.replace("\r\n", "\n");
                let line = self.line_clip.as_deref() == Some(norm.as_str());
                self.cur().doc.paste(&norm, line);
            }
            Action::Copy | Action::Cut => {
                let (text, line) = if matches!(a, Action::Cut) { self.cur().doc.cut() } else { self.cur().doc.copy() };
                self.store_clip(ctx, text, line);
            }
            Action::Cmd(cmd) => {
                if cmd.is_global() {
                    return self.run_global(ctx, cmd);
                }
                if cmd == Cmd::Escape && self.find.open {
                    self.find.open = false;
                    return;
                }
                let doc = &mut self.tabs[self.active].doc;
                use Cmd::*;
                match cmd {
                    Undo => doc.undo(),
                    Redo => doc.redo(),
                    SelectAll => doc.select_all(),
                    SelectLine => doc.select_line(),
                    ExpandSelection => doc.expand_selection(),
                    ShrinkSelection => doc.shrink_selection(),
                    SelectNextOccurrence => doc.select_next_occurrence(),
                    UnselectOccurrence => doc.unselect_occurrence(),
                    SelectAllOccurrences => doc.select_all_occurrences(),
                    SplitIntoLines => doc.split_into_lines(),
                    AddCaretAbove => doc.add_caret(true),
                    AddCaretBelow => doc.add_caret(false),
                    CaretsAtLineEnds => doc.carets_at_line_ends(),
                    Escape => doc.single_selection(),
                    Duplicate => doc.duplicate(),
                    DeleteLine => doc.delete_lines(),
                    MoveLineUp => doc.move_lines(true),
                    MoveLineDown => doc.move_lines(false),
                    ToggleComment => doc.toggle_comment(),
                    Indent => doc.indent(),
                    Outdent => doc.outdent(),
                    JoinLines => doc.join_lines(),
                    SortLines => doc.sort_lines(),
                    ToggleCase => doc.toggle_case(),
                    InsertLineBelow => doc.insert_line(false),
                    InsertLineAbove => doc.insert_line(true),
                    GotoBracket => doc.goto_matching_bracket(),
                    Move(m, ext) => {
                        let m = match m {
                            Motion::PageUp(_) => Motion::PageUp(page),
                            Motion::PageDown(_) => Motion::PageDown(page),
                            m => m,
                        };
                        doc.move_sels(m, ext);
                    }
                    Backspace(w) => doc.backspace(w),
                    DeleteFwd(w) => doc.delete_forward(w),
                    Newline => doc.newline(),
                    Tab => doc.tab(),
                    _ => {}
                }
            }
        }
        self.cur().view.scroll_to_caret = true;
    }

    fn run_global(&mut self, ctx: &egui::Context, cmd: Cmd) {
        use Cmd::*;
        match cmd {
            NewFile => self.new_tab(),
            OpenFile => {
                if let Some(paths) = rfd::FileDialog::new().pick_files() {
                    for p in paths {
                        self.open_path(&p);
                    }
                }
            }
            OpenFolder => {
                if let Some(d) = rfd::FileDialog::new().pick_folder() {
                    self.set_root(d);
                }
            }
            Save => {
                self.save(self.active);
            }
            SaveAs => {
                self.save_as(self.active);
            }
            SaveAll => {
                for i in 0..self.tabs.len() {
                    if self.tabs[i].doc.is_dirty() {
                        self.save(i);
                    }
                }
            }
            CloseTab => self.close_tab(self.active),
            NextTab => self.active = (self.active + 1) % self.tabs.len(),
            PrevTab => self.active = (self.active + self.tabs.len() - 1) % self.tabs.len(),
            CommandPalette => self.open_palette(PMode::Commands, ""),
            GotoAnything => self.open_palette(PMode::Files, ""),
            GotoLine => self.open_palette(PMode::Files, ":"),
            SelectTheme => self.open_palette(PMode::Themes, ""),
            SetSyntax => self.open_palette(PMode::Syntaxes, ""),
            PasteFromHistory => self.open_palette(PMode::Clipboard, ""),
            Find | Replace => {
                let doc = &self.tabs[self.active].doc;
                let s = doc.primary_sel();
                if !s.is_empty() && doc.rope.char_to_line(s.start()) == doc.rope.char_to_line(s.end()) {
                    self.find.query = doc.rope.slice(s.start()..s.end()).to_string();
                }
                self.find.origin = s.start();
                self.find.open = true;
                self.find.replace = cmd == Replace;
                self.find.focus = true;
                self.find.key = None;
            }
            FindNext => self.find_step(true),
            FindPrev => self.find_step(false),
            ToggleSidebar => self.sidebar = !self.sidebar,
            ToggleMinimap => self.settings.minimap = !self.settings.minimap,
            ZoomIn => self.settings.font_size = (self.settings.font_size + 1.0).min(48.0),
            ZoomOut => self.settings.font_size = (self.settings.font_size - 1.0).max(6.0),
            ZoomReset => self.settings.font_size = self.default_font,
            ToggleCamelHumps => {
                self.camel_humps = !self.camel_humps;
                for t in &mut self.tabs {
                    t.doc.camel_humps = self.camel_humps;
                }
                let msg = format!("CamelHumps word movement {}", if self.camel_humps { "ON" } else { "OFF" });
                self.set_status(ctx, msg);
            }
            ToggleIndentation => {
                let d = &mut self.tabs[self.active].doc;
                d.use_tabs = !d.use_tabs;
            }
            ToggleColumnMode => {
                let t = &mut self.tabs[self.active];
                t.doc.toggle_column_mode();
                let on = t.doc.column_mode;
                self.set_status(ctx, format!("Column selection mode {}", if on { "ON" } else { "OFF" }));
            }
            ToggleLineEndings => {
                let d = &mut self.tabs[self.active].doc;
                d.crlf = !d.crlf;
            }
            Quit => ctx.send_viewport_cmd(ViewportCommand::Close),
            _ => {}
        }
    }

    // ------------------------------------------------------------ keyboard dispatch

    fn handle_input(&mut self, ctx: &egui::Context) {
        let editor_focused = ctx.memory(|m| m.has_focus(self.editor_id));
        let modal = self.palette.is_some() || self.pending.is_some();
        let events = ctx.input(|i| i.events.clone());
        let mut keep = Vec::with_capacity(events.len());
        let mut actions = vec![];
        let mut suppress_text = false;
        // IntelliJ "clone caret": press Ctrl twice, keep it held, then Up/Down.
        // egui repaints on every modifier change, so sampling per frame sees each press.
        let (mods, now) = ctx.input(|i| (i.modifiers, i.time));
        if mods.ctrl && !self.ctrl_prev {
            self.double_ctrl = !mods.shift && !mods.alt && now - self.ctrl_last_press < 0.4;
            self.ctrl_last_press = now;
        }
        if !mods.ctrl {
            self.double_ctrl = false;
        }
        self.ctrl_prev = mods.ctrl;
        for ev in events {
            let ed = editor_focused && !modal;
            match &ev {
                Event::Key { key: k @ (Key::ArrowUp | Key::ArrowDown), pressed: true, modifiers, .. }
                    if ed && self.double_ctrl && modifiers.ctrl && !modifiers.shift && !modifiers.alt =>
                {
                    actions.push(Action::Cmd(if *k == Key::ArrowUp { Cmd::AddCaretAbove } else { Cmd::AddCaretBelow }));
                }
                Event::Key { key, pressed: true, modifiers, .. } => {
                    if let Some(cmd) = keymap::lookup(*key, *modifiers) {
                        if (cmd.is_global() && self.pending.is_none()) || ed {
                            actions.push(Action::Cmd(cmd));
                            suppress_text = modifiers.alt || modifiers.ctrl;
                            continue;
                        }
                    }
                    keep.push(ev);
                }
                Event::Text(t) if ed => {
                    if !suppress_text {
                        actions.push(Action::Text(t.clone()));
                    }
                    suppress_text = false;
                }
                Event::Ime(ImeEvent::Commit(t)) if ed => actions.push(Action::Text(t.clone())),
                // egui-winit turns Shift+Insert into a Paste event and swallows the key, so
                // IntelliJ's Alt+Shift+Insert (column mode) arrives as Paste with Alt held.
                Event::Paste(_) if mods.alt && mods.shift => {
                    if self.pending.is_none() {
                        actions.push(Action::Cmd(Cmd::ToggleColumnMode));
                    }
                }
                Event::Paste(_) | Event::Copy | Event::Cut if mods.alt => {}
                Event::Paste(t) if ed => actions.push(Action::Paste(t.clone())),
                Event::Copy if ed => actions.push(Action::Copy),
                Event::Cut if ed => actions.push(Action::Cut),
                _ => keep.push(ev),
            }
        }
        ctx.input_mut(|i| i.events = keep);
        for a in actions {
            self.run(ctx, a);
        }
        // Ctrl+wheel zoom
        let zoom = ctx.input(|i| i.zoom_delta());
        if zoom != 1.0 {
            self.settings.font_size = (self.settings.font_size * zoom).clamp(6.0, 48.0);
        }
        // Drag & drop
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        for p in dropped {
            self.open_path(&p);
        }
    }

    // ------------------------------------------------------------ find

    fn refresh_find(&mut self) {
        if !self.find.open {
            self.find.matches.clear();
            self.find.key = None;
            return;
        }
        let doc = &self.tabs[self.active].doc;
        let f = &self.find;
        let key = (doc.id, doc.version, f.query.clone(), f.case, f.whole, f.regex);
        if f.key.as_ref() == Some(&key) {
            return;
        }
        self.find.matches = search::find_all(&doc.rope, &doc.rope.to_string(), &f.query, f.case, f.whole, f.regex);
        self.find.key = Some(key);
    }

    fn find_step(&mut self, forward: bool) {
        if !self.find.open && !self.find.query.is_empty() {
            self.find.open = true;
        }
        self.refresh_find();
        let m = &self.find.matches;
        if m.is_empty() {
            return;
        }
        let tab = &mut self.tabs[self.active];
        let s = tab.doc.primary_sel();
        let target = if forward {
            m.iter().find(|x| x.0 >= s.end()).or(m.first())
        } else {
            m.iter().rev().find(|x| x.1 <= s.start()).or(m.last())
        };
        if let Some(&(a, b)) = target {
            tab.doc.set_sels(vec![Sel::new(a, b)], 0);
            tab.view.scroll_to_caret = true;
        }
    }

    fn find_from_origin(&mut self) {
        self.refresh_find();
        let origin = self.find.origin;
        if let Some(&(a, b)) = self.find.matches.iter().find(|m| m.0 >= origin).or(self.find.matches.first()) {
            let tab = &mut self.tabs[self.active];
            tab.doc.set_sels(vec![Sel::new(a, b)], 0);
            tab.view.scroll_to_caret = true;
        }
    }

    fn find_select_all(&mut self, ctx: &egui::Context) {
        self.refresh_find();
        if self.find.matches.is_empty() {
            return;
        }
        let sels = self.find.matches.iter().map(|&(a, b)| Sel::new(a, b)).collect();
        let tab = &mut self.tabs[self.active];
        tab.doc.set_sels(sels, 0);
        tab.view.scroll_to_caret = true;
        self.find.open = false;
        self.focus_editor(ctx);
    }

    fn replace_one(&mut self) {
        self.refresh_find();
        let s = self.tabs[self.active].doc.primary_sel();
        if self.find.matches.iter().any(|m| m.0 == s.start() && m.1 == s.end()) && !s.is_empty() {
            let r = self.find.replacement.clone();
            let doc = &mut self.tabs[self.active].doc;
            doc.set_sels(vec![s], 0);
            doc.replace_selections(&r);
        }
        self.find_step(true);
    }

    fn replace_all(&mut self, ctx: &egui::Context) {
        self.refresh_find();
        let n = self.find.matches.len();
        if n == 0 {
            return;
        }
        let sels = self.find.matches.iter().map(|&(a, b)| Sel::new(a, b)).collect();
        let r = self.find.replacement.clone();
        let doc = &mut self.tabs[self.active].doc;
        doc.set_sels(sels, 0);
        doc.replace_selections(&r);
        self.set_status(ctx, format!("Replaced {n} occurrences"));
    }

    fn find_bar(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let qid = Id::new("find_query");
        let rid = Id::new("find_replace");
        let q_focus = ctx.memory(|m| m.has_focus(qid));
        let r_focus = ctx.memory(|m| m.has_focus(rid));
        if q_focus || r_focus {
            let (esc, alt_enter, shift_enter, enter) = ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::Escape),
                    i.consume_key(Modifiers::ALT, Key::Enter),
                    i.consume_key(Modifiers::SHIFT, Key::Enter),
                    i.consume_key(Modifiers::NONE, Key::Enter),
                )
            });
            if esc {
                self.find.open = false;
                self.focus_editor(ctx);
                return;
            }
            if q_focus {
                if alt_enter {
                    return self.find_select_all(ctx);
                }
                if shift_enter {
                    self.find_step(false);
                }
                if enter {
                    self.find_step(true);
                }
            } else if enter {
                self.replace_one();
            } else if alt_enter {
                self.replace_all(ctx);
            }
        }
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label("Find:");
            let r = ui.add(
                TextEdit::singleline(&mut self.find.query)
                    .id(qid)
                    .desired_width(320.0)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("Enter: next   Shift+Enter: prev   Alt+Enter: select all"),
            );
            if self.find.focus {
                r.request_focus();
                select_all_text(ctx, qid, &self.find.query);
                self.find.focus = false;
            }
            changed |= r.changed();
            changed |= ui.toggle_value(&mut self.find.case, "Aa").on_hover_text("Match case").changed();
            changed |= ui.toggle_value(&mut self.find.whole, "W").on_hover_text("Whole word").changed();
            changed |= ui.toggle_value(&mut self.find.regex, ".*").on_hover_text("Regular expression").changed();
            let n = self.find.matches.len();
            ui.label(if self.find.query.is_empty() {
                String::new()
            } else if n == 0 {
                "No results".into()
            } else {
                format!("{n} matches")
            });
            if ui.button("◀").on_hover_text("Find previous (Shift+F3)").clicked() {
                self.find_step(false);
            }
            if ui.button("▶").on_hover_text("Find next (F3)").clicked() {
                self.find_step(true);
            }
            if ui.button("Find All").clicked() {
                self.find_select_all(ctx);
            }
            ui.toggle_value(&mut self.find.replace, "Replace…");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("✕").clicked() {
                    self.find.open = false;
                }
            });
        });
        if self.find.replace {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.label("With:");
                ui.add(
                    TextEdit::singleline(&mut self.find.replacement)
                        .id(rid)
                        .desired_width(320.0)
                        .font(egui::TextStyle::Monospace),
                );
                if ui.button("Replace").clicked() {
                    self.replace_one();
                }
                if ui.button("Replace All").clicked() {
                    self.replace_all(ctx);
                }
            });
        }
        if changed {
            self.find_from_origin();
        }
    }

    // ------------------------------------------------------------ palette

    fn open_palette(&mut self, mode: PMode, query: &str) {
        self.palette = Some(Palette { mode, query: query.into(), selected: 0, focus: true, cache: None });
    }

    fn palette_items(&self, mode: PMode, query: &str) -> Vec<PItem> {
        let mut items: Vec<(i64, PItem)> = vec![];
        let mut push = |label: String, detail: String, action: PAction, q: &str| {
            if let Some((score, hits)) = search::fuzzy(q, &label) {
                items.push((score, PItem { label, detail, hits, action }));
            }
        };
        match mode {
            PMode::Commands => {
                for &c in Cmd::palette() {
                    push(c.name().into(), keymap::shortcut(c), PAction::Cmd(c), query);
                }
            }
            PMode::Files => {
                if let Some(rest) = query.strip_prefix(':') {
                    let n = rest.trim().parse::<usize>().ok();
                    let total = self.tabs[self.active].doc.rope.len_lines();
                    return vec![PItem {
                        label: match n {
                            Some(n) => format!("Go to line {n}"),
                            None => format!("Type a line number (1–{total})"),
                        },
                        detail: String::new(),
                        hits: vec![],
                        action: PAction::Line(n.unwrap_or(0)),
                    }];
                }
                let mut seen = std::collections::HashSet::new();
                for t in &self.tabs {
                    if let Some(p) = &t.doc.path {
                        seen.insert(p.clone());
                        push(self.rel(p), "open".into(), PAction::Open(p.clone()), query);
                    }
                }
                for p in &self.files {
                    if !seen.contains(p) {
                        push(self.rel(p), String::new(), PAction::Open(p.clone()), query);
                    }
                }
            }
            PMode::Themes => {
                for name in self.syn.themes.themes.keys() {
                    let d = if *name == self.theme_name { "current" } else { "" };
                    push(name.clone(), d.into(), PAction::Theme(name.clone()), query);
                }
            }
            PMode::Syntaxes => {
                for s in self.syn.ss.syntaxes() {
                    if s.hidden {
                        continue;
                    }
                    push(s.name.clone(), s.file_extensions.join(", "), PAction::Syntax(s.name.clone()), query);
                }
            }
            PMode::Clipboard => {
                for (i, h) in self.clip_history.iter().enumerate() {
                    let first: String = h.lines().next().unwrap_or("").chars().take(100).collect();
                    let n = h.lines().count();
                    let d = if n > 1 { format!("{n} lines") } else { String::new() };
                    push(first, d, PAction::Clip(i), query);
                }
            }
        }
        if !query.is_empty() && mode != PMode::Clipboard {
            items.sort_by(|a, b| b.0.cmp(&a.0));
        }
        items.into_iter().take(300).map(|x| x.1).collect()
    }

    fn rel(&self, p: &Path) -> String {
        self.root
            .as_ref()
            .and_then(|r| p.strip_prefix(r).ok())
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn palette_ui(&mut self, ctx: &egui::Context) {
        let Some(mut p) = self.palette.take() else { return };
        let (up, down, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if esc {
            self.focus_editor(ctx);
            return;
        }
        if p.cache.as_ref().map(|c| c.0 != p.query).unwrap_or(true) {
            let items = self.palette_items(p.mode, &p.query);
            p.cache = Some((p.query.clone(), items));
            p.selected = 0;
        }
        let n = p.cache.as_ref().map_or(0, |c| c.1.len());
        if down && n > 0 {
            p.selected = (p.selected + 1) % n;
        }
        if up && n > 0 {
            p.selected = (p.selected + n - 1) % n;
        }
        let mut chosen = if enter && n > 0 { Some(p.selected) } else { None };
        let pid = Id::new("palette_query");
        let mut close = false;
        let placeholder = match p.mode {
            PMode::Commands => "Type a command…",
            PMode::Files => "Search files by name (prefix : to go to a line)",
            PMode::Themes => "Select color theme…",
            PMode::Syntaxes => "Set syntax…",
            PMode::Clipboard => "Paste from history…",
        };
        let colors = self.colors.clone();
        egui::Area::new(Id::new("palette"))
            .anchor(Align2::CENTER_TOP, vec2(0.0, 48.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                Frame::popup(ui.style()).inner_margin(Margin::same(8.0)).show(ui, |ui| {
                    ui.set_width(620.0);
                    let r = ui.add(
                        TextEdit::singleline(&mut p.query)
                            .id(pid)
                            .desired_width(f32::INFINITY)
                            .font(FontId::proportional(16.0))
                            .hint_text(placeholder),
                    );
                    if p.focus {
                        r.request_focus();
                        let len = p.query.chars().count();
                        set_cursor_end(ctx, pid, len);
                        p.focus = false;
                    }
                    if r.lost_focus() && !enter && ctx.input(|i| i.pointer.any_pressed()) {
                        close = true;
                    }
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                        let items = &p.cache.as_ref().unwrap().1;
                        for (i, it) in items.iter().enumerate() {
                            let sel = i == p.selected;
                            let mut job = LayoutJob::default();
                            let base = if sel { colors.fg } else { lerp(colors.panel, colors.fg, 0.8) };
                            let hit = Color32::from_rgb(110, 180, 255);
                            for (ci, ch) in it.label.chars().enumerate() {
                                let col = if it.hits.contains(&ci) { hit } else { base };
                                job.append(
                                    &ch.to_string(),
                                    0.0,
                                    TextFormat { font_id: FontId::proportional(14.0), color: col, ..Default::default() },
                                );
                            }
                            let resp = ui
                                .horizontal(|ui| {
                                    ui.set_width(ui.available_width());
                                    let r = ui.selectable_label(sel, job);
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui.label(RichText::new(&it.detail).weak().monospace());
                                    });
                                    r
                                })
                                .inner;
                            if sel && (up || down) {
                                resp.scroll_to_me(None);
                            }
                            if resp.clicked() {
                                chosen = Some(i);
                            }
                        }
                    });
                });
            });
        if let Some(i) = chosen {
            let action = p.cache.as_ref().unwrap().1.get(i).map(|x| x.action.clone());
            if let Some(a) = action {
                self.focus_editor(ctx);
                self.palette_action(ctx, a);
            }
            return;
        }
        if !close {
            self.palette = Some(p);
        }
    }

    fn palette_action(&mut self, ctx: &egui::Context, a: PAction) {
        match a {
            PAction::Cmd(c) => self.run(ctx, Action::Cmd(c)),
            PAction::Open(p) => self.open_path(&p),
            PAction::Line(n) => {
                if n > 0 {
                    let t = self.cur();
                    t.doc.goto_line(n);
                    t.view.center_caret = true;
                }
            }
            PAction::Theme(name) => {
                self.theme_name = name;
                self.apply_visuals(ctx);
                for t in &mut self.tabs {
                    t.view.cache.reset();
                }
            }
            PAction::Syntax(name) => {
                let t = self.cur();
                t.view.cache = LineCache::new(name);
            }
            PAction::Clip(i) => {
                if let Some(t) = self.clip_history.get(i).cloned() {
                    ctx.output_mut(|o| o.copied_text = t.clone());
                    self.run(ctx, Action::Paste(t));
                }
            }
        }
    }

    // ------------------------------------------------------------ sidebar

    fn tree_ui(&mut self, ui: &mut egui::Ui, dir: &Path, active: Option<&Path>) {
        let children = self.dir_cache.entry(dir.to_path_buf()).or_insert_with(|| list_dir(dir)).clone();
        for (path, is_dir) in children {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if is_dir {
                egui::CollapsingHeader::new(RichText::new(&name))
                    .id_salt(&path)
                    .default_open(false)
                    .show(ui, |ui| self.tree_ui(ui, &path, active));
            } else {
                let is_active = active == Some(path.as_path());
                let r = ui.add(egui::SelectableLabel::new(is_active, RichText::new(&name)));
                if r.clicked() {
                    self.to_open = Some(path.clone());
                }
                r.on_hover_text(path.to_string_lossy());
            }
        }
    }

    // ------------------------------------------------------------ dialogs

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(p) = &self.pending else { return };
        let (msg, dirty): (String, Vec<usize>) = match p {
            Pending::CloseTab(i) => (format!("Save changes to {}?", self.tabs[*i].doc.name()), vec![*i]),
            Pending::Quit => {
                let d: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].doc.is_dirty()).collect();
                (format!("{} file(s) have unsaved changes. Save them before quitting?", d.len()), d)
            }
        };
        let mut choice = None;
        egui::Window::new("Unsaved changes")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.label(msg);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                        choice = Some(0);
                    }
                    if ui.button("Don't Save").clicked() {
                        choice = Some(1);
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                        choice = Some(2);
                    }
                });
            });
        let Some(choice) = choice else { return };
        let pending = self.pending.take().unwrap();
        match (pending, choice) {
            (_, 2) => {}
            (Pending::CloseTab(i), 0) => {
                if self.save(i) {
                    self.force_close(i);
                }
            }
            (Pending::CloseTab(i), _) => self.force_close(i),
            (Pending::Quit, c) => {
                let ok = c == 1 || dirty.iter().all(|&i| self.save(i));
                if ok {
                    self.allow_quit = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }
        self.focus_editor(ctx);
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested()) {
            if !self.allow_quit && self.tabs.iter().any(|t| t.doc.is_dirty()) {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.pending = Some(Pending::Quit);
            } else {
                self.persist_session(ctx, true);
            }
        }

        self.handle_input(ctx);
        self.refresh_find();
        let c = self.colors.clone();

        // ---------------- menu
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                let item = |ui: &mut egui::Ui, cmd: Cmd, app: &mut App| {
                    let r = ui.add(egui::Button::new(cmd.name().split(": ").last().unwrap_or("")).shortcut_text(keymap::shortcut(cmd)));
                    if r.clicked() {
                        ui.close_menu();
                        app.run(ctx, Action::Cmd(cmd));
                        app.focus_editor(ctx);
                    }
                };
                ui.menu_button("File", |ui| {
                    for cmd in [Cmd::NewFile, Cmd::OpenFile, Cmd::OpenFolder, Cmd::Save, Cmd::SaveAs, Cmd::SaveAll, Cmd::CloseTab, Cmd::Quit] {
                        item(ui, cmd, self);
                    }
                });
                ui.menu_button("Edit", |ui| {
                    for cmd in [Cmd::Undo, Cmd::Redo, Cmd::PasteFromHistory, Cmd::Duplicate, Cmd::DeleteLine, Cmd::MoveLineUp, Cmd::MoveLineDown, Cmd::ToggleComment, Cmd::JoinLines, Cmd::SortLines, Cmd::ToggleCase, Cmd::Indent, Cmd::Outdent] {
                        item(ui, cmd, self);
                    }
                });
                ui.menu_button("Selection", |ui| {
                    for cmd in [Cmd::SelectAll, Cmd::SelectLine, Cmd::ExpandSelection, Cmd::ShrinkSelection, Cmd::SelectNextOccurrence, Cmd::UnselectOccurrence, Cmd::SelectAllOccurrences, Cmd::SplitIntoLines, Cmd::AddCaretAbove, Cmd::AddCaretBelow, Cmd::CaretsAtLineEnds, Cmd::ToggleColumnMode] {
                        item(ui, cmd, self);
                    }
                });
                ui.menu_button("Find", |ui| {
                    for cmd in [Cmd::Find, Cmd::Replace, Cmd::FindNext, Cmd::FindPrev, Cmd::GotoAnything, Cmd::GotoLine, Cmd::GotoBracket] {
                        item(ui, cmd, self);
                    }
                });
                ui.menu_button("View", |ui| {
                    for cmd in [Cmd::CommandPalette, Cmd::ToggleSidebar, Cmd::ToggleMinimap, Cmd::ZoomIn, Cmd::ZoomOut, Cmd::ZoomReset, Cmd::SelectTheme, Cmd::SetSyntax, Cmd::ToggleCamelHumps] {
                        item(ui, cmd, self);
                    }
                    if let Some(dir) = config_dir() {
                        if ui.button("Open Config Folder (themes/syntaxes)").clicked() {
                            let _ = std::fs::create_dir_all(dir.join("themes"));
                            let _ = std::fs::create_dir_all(dir.join("syntaxes"));
                            let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                            ui.close_menu();
                        }
                    }
                });
            });
        });

        // ---------------- status bar
        egui::TopBottomPanel::bottom("status").frame(Frame::none().fill(c.panel).inner_margin(Margin::symmetric(8.0, 3.0))).show(ctx, |ui| {
            ui.horizontal(|ui| {
                let t = &self.tabs[self.active];
                let d = &t.doc;
                let s = d.primary_sel();
                let (l, col) = crate::text::pos_to_lc(&d.rope, s.head);
                let mut left = format!("Line {}, Column {}", l + 1, col + 1);
                if d.sels.len() > 1 {
                    left += &format!("  ·  {} selections", d.sels.len());
                } else if !s.is_empty() {
                    left += &format!("  ·  {} characters selected", s.end() - s.start());
                }
                ui.label(RichText::new(left).small());
                if let Some((msg, _)) = &self.status {
                    ui.separator();
                    ui.label(RichText::new(msg).small().strong());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(&t.view.cache.syntax).small());
                    ui.separator();
                    ui.label(RichText::new(if d.crlf { "CRLF" } else { "LF" }).small());
                    ui.separator();
                    ui.label(RichText::new(if d.use_tabs { format!("Tab Size: {}", d.tab_size) } else { format!("Spaces: {}", d.tab_size) }).small());
                    ui.separator();
                    if d.column_mode {
                        ui.label(RichText::new("Column").small().strong())
                            .on_hover_text("Column selection mode (Alt+Shift+Insert)");
                        ui.separator();
                    }
                    ui.label(RichText::new(if self.camel_humps { "CamelHumps" } else { "Words" }).small())
                        .on_hover_text("Ctrl+Left/Right word movement mode (toggle in View menu)");
                });
            });
        });
        if self.status.as_ref().is_some_and(|(_, t)| *t > 0.0 && ctx.input(|i| i.time) - t > 4.0) {
            self.status = None;
        }

        // ---------------- sidebar
        if self.sidebar {
            egui::SidePanel::left("sidebar").resizable(true).default_width(230.0).frame(Frame::none().fill(c.panel).inner_margin(Margin::same(6.0))).show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.label(RichText::new("OPEN FILES").small().weak());
                    let mut activate = None;
                    for (i, t) in self.tabs.iter().enumerate() {
                        let name = if t.doc.is_dirty() { format!("● {}", t.doc.name()) } else { t.doc.name() };
                        if ui.selectable_label(i == self.active, name).clicked() {
                            activate = Some(i);
                        }
                    }
                    if let Some(i) = activate {
                        self.active = i;
                        self.focus_editor(ctx);
                    }
                    ui.add_space(8.0);
                    if let Some(root) = self.root.clone() {
                        ui.horizontal(|ui| {
                            let name = root.file_name().map(|n| n.to_string_lossy().to_uppercase()).unwrap_or_default();
                            ui.label(RichText::new(format!("FOLDER · {name}")).small().weak());
                            if ui.small_button("⟳").on_hover_text("Refresh").clicked() {
                                self.dir_cache.clear();
                                self.files = walk(&root, 50_000);
                            }
                        });
                        let active = self.tabs[self.active].doc.path.clone();
                        self.tree_ui(ui, &root, active.as_deref());
                    } else if ui.button("Open Folder…").clicked() {
                        self.run_global(ctx, Cmd::OpenFolder);
                    }
                });
            });
        }
        if let Some(p) = self.to_open.take() {
            self.open_path(&p);
            self.focus_editor(ctx);
        }

        // ---------------- tabs
        egui::TopBottomPanel::top("tabs").frame(Frame::none().fill(c.panel)).show(ctx, |ui| {
            egui::ScrollArea::horizontal().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 1.0;
                    let mut activate = None;
                    let mut close = None;
                    for (i, t) in self.tabs.iter().enumerate() {
                        let sel = i == self.active;
                        let fill = if sel { c.bg } else { lerp(c.panel, c.bg, 0.4) };
                        let text_col = if sel { c.fg } else { lerp(c.panel, c.fg, 0.6) };
                        Frame::none().fill(fill).inner_margin(Margin::symmetric(12.0, 6.0)).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let r = ui.add(egui::Label::new(RichText::new(t.doc.name()).color(text_col)).sense(Sense::click()).selectable(false));
                                if r.clicked() {
                                    activate = Some(i);
                                }
                                if r.middle_clicked() {
                                    close = Some(i);
                                }
                                if let Some(p) = &t.doc.path {
                                    r.on_hover_text(p.to_string_lossy());
                                }
                                let x = ui.add(egui::Label::new(RichText::new(if t.doc.is_dirty() { "●" } else { "×" }).color(text_col)).sense(Sense::click()).selectable(false));
                                if x.clicked() {
                                    close = Some(i);
                                }
                            });
                        });
                    }
                    if let Some(i) = activate {
                        self.active = i;
                        self.focus_editor(ctx);
                    }
                    if let Some(i) = close {
                        self.close_tab(i);
                    }
                });
            });
        });

        // ---------------- find bar
        if self.find.open {
            egui::TopBottomPanel::bottom("find").frame(Frame::none().fill(c.panel).inner_margin(Margin::symmetric(4.0, 6.0))).show(ctx, |ui| {
                self.find_bar(ctx, ui);
            });
        }
        self.refresh_find();

        // ---------------- editor
        egui::CentralPanel::default().frame(Frame::none().fill(c.bg)).show(ctx, |ui| {
            let theme = self.syn.theme(&self.theme_name);
            let find: &[(usize, usize)] = if self.find.open { &self.find.matches } else { &[] };
            let cx = editor::Ctx { syn: &self.syn, theme, colors: &self.colors, settings: &self.settings, find, id: self.editor_id };
            let tab = &mut self.tabs[self.active];
            let resp = editor::show(ui, &mut tab.doc, &mut tab.view, &cx);
            if std::mem::take(&mut self.startup_focus) {
                resp.request_focus();
            }
        });

        self.palette_ui(ctx);
        self.dialogs(ctx);

        // ---------------- window title
        let d = &self.tabs[self.active].doc;
        let title = format!(
            "{}{}{} — edit",
            if d.is_dirty() { "● " } else { "" },
            d.name(),
            self.root.as_ref().and_then(|r| r.file_name()).map(|n| format!(" · {}", n.to_string_lossy())).unwrap_or_default()
        );
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        self.persist_session(ctx, false);
    }
}

// ---------------------------------------------------------------- helpers

fn select_all_text(ctx: &egui::Context, id: Id, text: &str) {
    if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
        let n = text.chars().count();
        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(n),
        )));
        state.store(ctx, id);
    }
}

fn set_cursor_end(ctx: &egui::Context, id: Id, n: usize) {
    if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(n))));
        state.store(ctx, id);
    }
}

const SKIP_DIRS: &[&str] = &[".git", ".hg", ".svn", "node_modules", "target", ".idea", ".vs", "__pycache__", ".gradle", "build", "dist", "out"];

fn list_dir(dir: &Path) -> Vec<(PathBuf, bool)> {
    let mut v: Vec<(PathBuf, bool)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| {
                    let p = e.path();
                    let d = p.is_dir();
                    (p, d)
                })
                .filter(|(p, d)| !(*d && p.file_name().is_some_and(|n| n == ".git")))
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| {
        b.1.cmp(&a.1).then_with(|| {
            a.0.file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .cmp(&b.0.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        })
    });
    v
}

fn walk(root: &Path, limit: usize) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                let name = e.file_name();
                if !SKIP_DIRS.iter().any(|s| name == *s) {
                    stack.push(p);
                }
            } else {
                out.push(p);
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }
    out.sort();
    out
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut candidates: Vec<PathBuf> = vec![];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let d = PathBuf::from(local).join("Microsoft").join("Windows").join("Fonts");
        candidates.push(d.join("JetBrainsMono-Regular.ttf"));
    }
    let win = PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into())).join("Fonts");
    candidates.push(win.join("JetBrainsMono-Regular.ttf"));
    candidates.push(win.join("CascadiaMono.ttf"));
    candidates.push(win.join("consola.ttf"));
    for p in candidates {
        if let Ok(bytes) = std::fs::read(&p) {
            fonts.font_data.insert("mono".into(), FontData::from_owned(bytes));
            fonts.families.get_mut(&FontFamily::Monospace).unwrap().insert(0, "mono".into());
            break;
        }
    }
    if let Ok(bytes) = std::fs::read(win.join("segoeui.ttf")) {
        fonts.font_data.insert("ui".into(), FontData::from_owned(bytes));
        fonts.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "ui".into());
    }
    ctx.set_fonts(fonts);
}
