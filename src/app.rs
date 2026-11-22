//! Application shell: tabs, sidebar, command palette, find bar, dialogs.

use crate::doc::{Document, Edit, EditKind, Motion};
use crate::diffview::{self, DiffState};
use crate::editor::{self, Settings, View};
use crate::highlight::{config_dir, lerp, Colors, LineCache, Syntax};
use crate::keymap::{self, Cmd};
use crate::findfiles;
use crate::search;
use crate::selection::Sel;
use crate::session::{self, FileState, Session};
use crate::text::{line_end, line_text};
use crate::watch::{self, FsWatch};
use eframe::egui::{
    self, text::LayoutJob, vec2, Align2, Color32, Event, FontData, FontDefinitions, FontFamily,
    FontId, Frame, Id, ImeEvent, Key, Margin, Modifiers, RichText, Sense, TextEdit, TextFormat,
    ViewportCommand,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub struct Tab {
    pub doc: Document,
    pub view: View,
    /// Side-by-side diff shown instead of the plain editor (the right side is `doc`).
    pub diff: Option<DiffState>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PMode {
    Commands,
    Files,
    Themes,
    Syntaxes,
    Clipboard,
    DiffTab,
}

#[derive(Clone)]
enum PAction {
    Cmd(Cmd),
    Open(PathBuf),
    Line(usize),
    Theme(String),
    Syntax(String),
    Clip(usize),
    DiffTab(usize),
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

/// Find in Files panel (Ctrl+Shift+F).
#[derive(Default)]
struct Fif {
    open: bool,
    q: findfiles::Query,
    focus: bool,
    search: Option<findfiles::Search>,
    results: Vec<findfiles::FileHits>,
    lines: usize,
    /// Files searched and whether the result limit was hit, once finished.
    done: Option<(usize, bool)>,
    error: Option<String>,
    /// Query changed at this time; searched after a short pause in typing.
    dirty_at: Option<f64>,
    /// Selected result: (file index, hit index).
    sel: Option<(usize, usize)>,
    scroll_to_sel: bool,
    /// Replace row shown (Ctrl+Shift+R).
    replace: bool,
    replacement: String,
    /// Replace All waiting for confirmation: (files open in tabs, files written to disk).
    confirm: Option<(usize, usize)>,
    /// Files Replace All wrote to disk: (path, before, after), for Undo Replace.
    undo: Vec<(PathBuf, String, String)>,
    /// After a replace, re-select this file and line once the new results arrive.
    resume: Option<(PathBuf, usize)>,
}

enum TreeAction {
    /// Diff two files: (left, right). The right one is opened as the editable side.
    Compare(PathBuf, PathBuf),
    /// Diff a file against the active editor.
    CompareWithActive(PathBuf),
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
    /// Folders shown in the sidebar (File › Open Folder replaces, Add Folder appends).
    roots: Vec<PathBuf>,
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
    diff_left_id: Id,
    /// The read-only left pane of a diff had keyboard focus when input was dispatched.
    left_focus: bool,
    status: Option<(String, f64)>,
    camel_humps: bool,
    to_open: Option<PathBuf>,
    /// Files selected in the sidebar tree (Ctrl+click adds), oldest first.
    tree_sel: Vec<PathBuf>,
    tree_action: Option<TreeAction>,
    /// Active file the tree selection last followed.
    tree_followed: Option<PathBuf>,
    /// Last session text written to disk, and when.
    session_text: String,
    session_saved_at: f64,
    /// Focus the editor once it exists (egui drops focus requests for unseen widgets).
    startup_focus: bool,
    ctrl_prev: bool,
    ctrl_last_press: f64,
    double_ctrl: bool,
    /// Unique per run, so backup names never collide with a previous run's files.
    run_tag: String,
    /// doc id -> version last written to its backup file.
    backed_up: HashMap<u64, u64>,
    /// Watches the project folder and the folders of open files.
    fs: FsWatch,
    /// When the Go to File list went stale (re-walked after 0.5s without changes).
    files_stale: Option<f64>,
    fif: Fif,
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
            tabs: vec![Tab { doc: Document::new(), view: View::new(plain), diff: None }],
            active: 0,
            syn,
            theme_name,
            colors,
            settings: Settings { font_size: 14.0, line_spacing: 1.35, minimap: true },
            default_font: 14.0,
            sidebar: true,
            roots: vec![],
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
            diff_left_id: Id::new("diff_left"),
            left_focus: false,
            status: None,
            camel_humps: true,
            to_open: None,
            tree_sel: vec![],
            tree_action: None,
            tree_followed: None,
            session_text: String::new(),
            session_saved_at: 0.0,
            startup_focus: false,
            ctrl_prev: false,
            ctrl_last_press: -1.0,
            double_ctrl: false,
            run_tag: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis().to_string())
                .unwrap_or_else(|_| std::process::id().to_string()),
            backed_up: HashMap::new(),
            fs: FsWatch::new(cc.egui_ctx.clone()),
            files_stale: None,
            fif: Fif::default(),
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
        if !had_session && app.roots.is_empty() && app.tabs.iter().all(|t| t.doc.path.is_none()) {
            app.sidebar = false;
        }
        app.startup_focus = true;
        app
    }

    // ------------------------------------------------------------ session

    fn restore(&mut self, s: Session) {
        if let Some(t) = s.theme.clone().filter(|t| self.syn.themes.themes.contains_key(t)) {
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
        for r in s.roots.iter().filter(|r| r.is_dir()) {
            self.add_root(r.clone());
        }
        let mut tab_of_entry: Vec<Option<usize>> = vec![];
        let mut unsaved = 0;
        for f in &s.files {
            let backup = f.backup.as_deref().and_then(session::read_backup);
            match (backup, &f.path) {
                // hot exit: unsaved contents (of a file or an untitled buffer)
                (Some(text), path) => {
                    let mut doc = Document::new();
                    doc.camel_humps = self.camel_humps;
                    doc.path = path.clone();
                    doc.load_backup(&text, f.crlf);
                    let first = line_text(&doc.rope, 0);
                    let syntax = self.syn.detect(path.as_deref(), &first);
                    self.push_tab(Tab { doc, view: View::new(syntax), diff: None });
                    unsaved += 1;
                }
                // clean file (or its backup is gone): reload from disk
                (None, Some(p)) if p.is_file() => self.open_path(p),
                _ => {
                    tab_of_entry.push(None);
                    continue;
                }
            }
            let t = self.cur();
            t.doc.set_sels(vec![Sel::new(f.anchor, f.head)], 0);
            t.view.scroll.y = f.scroll_y;
            tab_of_entry.push(Some(self.active));
        }
        if let Some(Some(i)) = tab_of_entry.get(s.active) {
            self.active = *i;
        }
        if let Some(b) = s.sidebar {
            self.sidebar = b;
        }
        if unsaved > 0 {
            self.status = Some((format!("Restored {unsaved} unsaved buffer(s)"), 0.0));
        }
    }

    fn backup_name(&self, doc: &Document) -> String {
        format!("{}-{}.txt", self.run_tag, doc.id)
    }

    /// Tabs that go into the session: files, and unsaved buffers (hot exit).
    fn session(&self) -> Session {
        let included = |t: &Tab| t.doc.path.is_some() || t.doc.is_dirty();
        Session {
            roots: self.roots.clone(),
            files: self
                .tabs
                .iter()
                .filter(|t| included(t))
                .map(|t| {
                    let s = t.doc.primary_sel();
                    FileState {
                        path: t.doc.path.clone(),
                        backup: t.doc.is_dirty().then(|| self.backup_name(&t.doc)),
                        crlf: t.doc.crlf,
                        anchor: s.anchor,
                        head: s.head,
                        scroll_y: t.view.scroll.y,
                    }
                })
                .collect(),
            active: self.tabs[..self.active].iter().filter(|t| included(t)).count(),
            theme: Some(self.theme_name.clone()),
            font_size: Some(self.settings.font_size),
            sidebar: Some(self.sidebar),
            minimap: Some(self.settings.minimap),
            camel_humps: Some(self.camel_humps),
        }
    }

    /// Save the session and unsaved-buffer backups when something changed: throttled to
    /// every 2s, immediately when `force`. Returns false if anything failed to save.
    fn persist_session(&mut self, ctx: &egui::Context, force: bool) -> bool {
        let pending: Vec<usize> = (0..self.tabs.len())
            .filter(|&i| {
                let d = &self.tabs[i].doc;
                d.is_dirty() && self.backed_up.get(&d.id) != Some(&d.version)
            })
            .collect();
        let session = self.session();
        let text = session.to_text();
        if text == self.session_text && pending.is_empty() {
            return true;
        }
        let now = ctx.input(|i| i.time);
        let since = now - self.session_saved_at;
        if !force && since < 2.0 {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(2.05 - since));
            return true;
        }
        let mut ok = true;
        for i in pending {
            let d = &self.tabs[i].doc;
            let (id, version, name) = (d.id, d.version, self.backup_name(d));
            match session::write_backup(&name, &d.rope.to_string()) {
                Ok(()) => {
                    self.backed_up.insert(id, version);
                }
                Err(e) => {
                    ok = false;
                    self.set_status(ctx, format!("Could not back up unsaved changes: {e}"));
                }
            }
        }
        match Session::save_text(&text) {
            Ok(()) => session::cleanup_backups(&session.backup_names()),
            Err(e) => {
                ok = false;
                self.set_status(ctx, format!("Could not save session: {e}"));
            }
        }
        self.session_text = text;
        self.session_saved_at = now;
        ok
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
        self.tabs.push(Tab { doc, view: View::new(plain), diff: None });
        self.active = self.tabs.len() - 1;
    }

    fn open_path(&mut self, p: &Path) {
        let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        let p = abs.as_path();
        if p.is_dir() {
            self.add_root(p.to_path_buf());
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
                self.push_tab(Tab { doc, view: View::new(syntax), diff: None });
            }
            Err(e) => self.status = Some((format!("Cannot open {}: {e}", p.display()), 0.0)),
        }
    }

    /// Add a tab and activate it, replacing the initial empty untitled tab.
    fn push_tab(&mut self, tab: Tab) {
        if self.tabs.len() == 1 && self.tabs[0].doc.is_pristine() {
            self.tabs[0] = tab;
            self.active = 0;
        } else {
            self.tabs.push(tab);
            self.active = self.tabs.len() - 1;
        }
    }

    /// Show only `dir` in the sidebar (File › Open Folder).
    fn set_root(&mut self, dir: PathBuf) {
        self.roots.clear();
        self.dir_cache.clear();
        self.tree_sel.clear();
        self.add_root(dir);
    }

    /// Add `dir` to the sidebar's folders (File › Add Folder, drag & drop, command line).
    fn add_root(&mut self, dir: PathBuf) {
        let dir = std::path::absolute(&dir).unwrap_or(dir);
        if !self.roots.contains(&dir) {
            self.roots.push(dir);
        }
        self.tree_followed = None;
        self.files = walk_roots(&self.roots);
        self.sidebar = true;
    }

    fn remove_root(&mut self, i: usize) {
        let dir = self.roots.remove(i);
        self.dir_cache.retain(|p, _| !p.starts_with(&dir) || self.roots.iter().any(|r| p.starts_with(r)));
        self.tree_sel.retain(|p| !p.starts_with(&dir));
        self.files = walk_roots(&self.roots);
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
        if let Some(dir) = self.tabs[i].doc.path.as_ref().and_then(|p| p.parent()).or(self.roots.first().map(PathBuf::as_path)) {
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
        if self.left_focus && self.tabs[self.active].diff.is_some() {
            return self.run_left(ctx, a);
        }
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
                {
                    // IntelliJ: Esc closes the diff (once there's nothing left to collapse)
                    let t = &mut self.tabs[self.active];
                    if cmd == Cmd::Escape && t.diff.is_some() && t.doc.sels.len() == 1 && t.doc.primary_sel().is_empty() {
                        t.diff = None;
                        return;
                    }
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
            AddFolder => {
                if let Some(dirs) = rfd::FileDialog::new().pick_folders() {
                    for d in dirs {
                        self.add_root(d);
                    }
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
            FindInFiles | ReplaceInFiles => {
                // IntelliJ: the selected text (one line) becomes the query
                let doc = &self.tabs[self.active].doc;
                let s = doc.primary_sel();
                if !s.is_empty() && doc.rope.char_to_line(s.start()) == doc.rope.char_to_line(s.end()) {
                    self.fif.q.text = doc.rope.slice(s.start()..s.end()).to_string();
                }
                self.fif.replace = cmd == ReplaceInFiles;
                self.fif.confirm = None;
                self.fif.open = true;
                self.fif.focus = true;
                self.fif.dirty_at = Some(0.0);
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
            DiffWithSaved | DiffWithHead | DiffWithClipboard | DiffWithFile => {
                if let Some((text, title)) = self.diff_source(cmd) {
                    self.open_diff(ctx, text, title);
                }
            }
            DiffTreeSelection => match self.tree_pair() {
                Some((a, b)) => self.run_tree_action(ctx, TreeAction::Compare(a, b)),
                None => self.set_status(ctx, "Ctrl+click two files in the sidebar first"),
            },
            ShowDiff => {
                if let Some((a, b)) = self.tree_pair() {
                    self.run_tree_action(ctx, TreeAction::Compare(a, b));
                } else if self.tabs[self.active].doc.path.is_none() {
                    self.set_status(ctx, "Nothing to compare: save the file or Ctrl+click two files in the sidebar");
                } else {
                    // unsaved edits -> compare with disk; otherwise with the last commit
                    let doc = &self.tabs[self.active].doc;
                    let src = if doc.is_dirty() { DiffWithSaved } else { DiffWithHead };
                    if let Some((text, title)) = self.diff_source(src) {
                        self.open_diff(ctx, text, title);
                    }
                }
            }
            DiffWithTab => {
                if self.tabs.len() > 1 {
                    self.open_palette(PMode::DiffTab, "");
                } else {
                    self.set_status(ctx, "Open another tab to compare with");
                }
            }
            CloseDiff => {
                self.tabs[self.active].diff = None;
                self.focus_editor(ctx);
            }
            NextDiff | PrevDiff => {
                let left_focus = self.left_focus;
                let t = &mut self.tabs[self.active];
                match t.diff.as_mut() {
                    Some(d) => d.navigate(&mut t.doc, &mut t.view, cmd == NextDiff, left_focus),
                    None => self.set_status(ctx, "Open a diff first (Diff menu)"),
                }
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
        let left_focused = self.tabs[self.active].diff.is_some() && ctx.memory(|m| m.has_focus(self.diff_left_id));
        self.left_focus = left_focused;
        let editor_focused = left_focused || ctx.memory(|m| m.has_focus(self.editor_id));
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
                if ui.button("×").clicked() {
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
            PMode::DiffTab => {
                for (i, t) in self.tabs.iter().enumerate() {
                    if i != self.active {
                        let d = t.doc.path.as_ref().map(|p| self.rel(p)).unwrap_or_default();
                        push(t.doc.name(), d, PAction::DiffTab(i), query);
                    }
                }
            }
        }
        if !query.is_empty() && mode != PMode::Clipboard {
            items.sort_by(|a, b| b.0.cmp(&a.0));
        }
        items.into_iter().take(300).map(|x| x.1).collect()
    }

    /// Path relative to its sidebar folder; prefixed with the folder name when there are
    /// several, so files with the same name in different folders can be told apart.
    fn rel(&self, p: &Path) -> String {
        let root = self.roots.iter().filter(|r| p.starts_with(r)).max_by_key(|r| r.as_os_str().len());
        let rel = root.and_then(|r| p.strip_prefix(r).ok()).unwrap_or(p).to_string_lossy().replace('\\', "/");
        match root.and_then(|r| r.file_name()) {
            Some(name) if self.roots.len() > 1 => format!("{}/{rel}", name.to_string_lossy()),
            _ => rel,
        }
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
            PMode::DiffTab => "Compare with tab…",
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
            PAction::DiffTab(i) => {
                if let Some(t) = self.tabs.get(i) {
                    let (text, title) = (t.doc.rope.to_string(), format!("Tab: {}", t.doc.name()));
                    self.open_diff(ctx, Ok(text), title);
                }
            }
        }
    }

    // ------------------------------------------------------------ diff

    /// The two most recently selected files in the tree, if at least two are selected.
    fn tree_pair(&self) -> Option<(PathBuf, PathBuf)> {
        let n = self.tree_sel.len();
        (n >= 2).then(|| (self.tree_sel[n - 2].clone(), self.tree_sel[n - 1].clone()))
    }

    /// Text of a file, preferring an open tab's buffer (so unsaved edits are compared).
    fn file_text(&self, p: &Path) -> Result<String, String> {
        if let Some(t) = self.tabs.iter().find(|t| t.doc.path.as_deref() == Some(p)) {
            return Ok(t.doc.rope.to_string());
        }
        std::fs::read(p)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .map_err(|e| format!("Cannot read {}: {e}", p.display()))
    }

    fn run_tree_action(&mut self, ctx: &egui::Context, a: TreeAction) {
        match a {
            TreeAction::Compare(left, right) => {
                let text = self.file_text(&left);
                if let Err(e) = text {
                    return self.set_status(ctx, e);
                }
                self.open_path(&right);
                if self.tabs[self.active].doc.path.as_deref() != Some(right.as_path()) {
                    return; // couldn't open (status already set)
                }
                let title = format!("File: {}", self.rel(&left));
                self.open_diff(ctx, text, title);
            }
            TreeAction::CompareWithActive(p) => {
                let title = format!("File: {}", self.rel(&p));
                let text = self.file_text(&p);
                self.open_diff(ctx, text, title);
            }
        }
    }

    fn open_diff(&mut self, ctx: &egui::Context, left: Result<String, String>, title: String) {
        match left {
            Ok(text) => {
                let tab = &mut self.tabs[self.active];
                tab.diff = Some(DiffState::new(&text, title, &tab.doc, &tab.view));
                self.find.open = false;
                self.focus_editor(ctx);
            }
            Err(e) => self.set_status(ctx, e),
        }
    }

    fn diff_source(&mut self, cmd: Cmd) -> Option<(Result<String, String>, String)> {
        let doc = &self.tabs[self.active].doc;
        let read = |p: &Path| std::fs::read(p).map(|b| String::from_utf8_lossy(&b).into_owned()).map_err(|e| e.to_string());
        Some(match cmd {
            Cmd::DiffWithSaved => match &doc.path {
                Some(p) => (read(p).map_err(|e| format!("Cannot read saved file: {e}")), format!("Saved: {}", doc.name())),
                None => (Err("This buffer has never been saved".into()), String::new()),
            },
            Cmd::DiffWithHead => match &doc.path {
                Some(p) => (git_head_text(p), format!("Git HEAD: {}", doc.name())),
                None => (Err("This buffer is not a file".into()), String::new()),
            },
            Cmd::DiffWithClipboard => (
                arboard::Clipboard::new()
                    .and_then(|mut c| c.get_text())
                    .map_err(|e| format!("Clipboard has no text: {e}")),
                "Clipboard".into(),
            ),
            Cmd::DiffWithFile => {
                let p = rfd::FileDialog::new().pick_file()?;
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                (read(&p), format!("File: {name}"))
            }
            _ => return None,
        })
    }

    /// Route input to the read-only left pane of a diff.
    fn run_left(&mut self, ctx: &egui::Context, a: Action) {
        if let Action::Cmd(cmd) = a {
            if cmd.is_global() {
                return self.run_global(ctx, cmd);
            }
        }
        let page = self.tabs[self.active].view.page_lines;
        let Some(d) = self.tabs[self.active].diff.as_mut() else { return };
        let doc = &mut d.left;
        let mut close = false;
        let mut clip = None;
        let mut read_only = false;
        match a {
            Action::Copy | Action::Cut => clip = Some(doc.copy()),
            Action::Cmd(cmd) => {
                use Cmd::*;
                match cmd {
                    Move(m, ext) => {
                        let m = match m {
                            Motion::PageUp(_) => Motion::PageUp(page),
                            Motion::PageDown(_) => Motion::PageDown(page),
                            m => m,
                        };
                        doc.move_sels(m, ext);
                    }
                    SelectAll => doc.select_all(),
                    SelectLine => doc.select_line(),
                    ExpandSelection => doc.expand_selection(),
                    ShrinkSelection => doc.shrink_selection(),
                    SelectNextOccurrence => doc.select_next_occurrence(),
                    SelectAllOccurrences => doc.select_all_occurrences(),
                    AddCaretAbove => doc.add_caret(true),
                    AddCaretBelow => doc.add_caret(false),
                    Escape if doc.sels.len() > 1 || !doc.primary_sel().is_empty() => doc.single_selection(),
                    Escape => close = true,
                    _ => read_only = true,
                }
            }
            Action::Text(_) | Action::Paste(_) => read_only = true,
        }
        d.left_view.scroll_to_caret = true;
        if let Some((t, line)) = clip {
            self.store_clip(ctx, t, line);
        }
        if close {
            self.tabs[self.active].diff = None;
            self.focus_editor(ctx);
        }
        if read_only {
            self.set_status(ctx, "The left side of the diff is read-only");
        }
    }

    // ------------------------------------------------------------ find in files

    /// Start a pending search after a pause in typing, and collect streamed results.
    fn fif_pump(&mut self, ctx: &egui::Context) {
        if let Some(t) = self.fif.dirty_at {
            let now = ctx.input(|i| i.time);
            if now - t >= 0.25 {
                self.fif.dirty_at = None;
                self.fif_start(ctx);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(0.26 - (now - t)));
            }
        }
        let Some(s) = &self.fif.search else { return };
        while let Ok(m) = s.rx.try_recv() {
            match m {
                findfiles::Msg::File(f) => {
                    self.fif.lines += f.hits.len();
                    let fi = self.fif.results.len();
                    // after a replace, continue from where we were
                    let resume = self.fif.resume.as_ref().filter(|(p, _)| *p == f.path).map(|&(_, l)| l);
                    if let Some(l) = resume {
                        let hi = f.hits.iter().position(|h| h.line >= l).unwrap_or(f.hits.len() - 1);
                        (self.fif.sel, self.fif.resume, self.fif.scroll_to_sel) = (Some((fi, hi)), None, true);
                    }
                    self.fif.results.push(f);
                }
                findfiles::Msg::Done(n, truncated) => self.fif.done = Some((n, truncated)),
            }
        }
        if self.fif.sel.is_none() && !self.fif.results.is_empty() && (self.fif.resume.is_none() || self.fif.done.is_some()) {
            self.fif.sel = Some((0, 0));
        }
        if self.fif.done.is_some() {
            self.fif.search = None;
        }
    }

    fn fif_start(&mut self, ctx: &egui::Context) {
        let f = &mut self.fif;
        (f.search, f.done, f.error, f.sel, f.lines) = (None, None, None, None, 0);
        f.results.clear();
        if f.q.text.is_empty() {
            return;
        }
        // all files in the sidebar folders, plus open files outside them
        let mut files = self.files.clone();
        let known: HashSet<&PathBuf> = self.files.iter().collect();
        let extra: Vec<PathBuf> =
            self.tabs.iter().filter_map(|t| t.doc.path.clone()).filter(|p| !known.contains(p)).collect();
        files.extend(extra);
        // unsaved edits are searched as they are in the editor, not as saved
        let buffers = self
            .tabs
            .iter()
            .filter(|t| t.doc.is_dirty())
            .filter_map(|t| Some((t.doc.path.clone()?, t.doc.rope.to_string())))
            .collect();
        match findfiles::start(files, buffers, &self.fif.q, ctx.clone()) {
            Ok(s) => self.fif.search = Some(s),
            Err(e) => self.fif.error = Some(e),
        }
    }

    /// Open the file of result (fi, hi) and select the match.
    fn fif_open(&mut self, fi: usize, hi: usize) {
        let Some((path, line, cols)) =
            self.fif.results.get(fi).and_then(|f| f.hits.get(hi).map(|h| (f.path.clone(), h.line, h.cols)))
        else {
            return;
        };
        self.fif.sel = Some((fi, hi));
        self.open_path(&path);
        let tab = self.cur();
        if tab.doc.path.as_deref() != Some(path.as_path()) {
            return;
        }
        let r = &tab.doc.rope;
        let l = line.min(r.len_lines() - 1);
        let (start, end) = (r.line_to_char(l), line_end(r, l));
        let (a, b) = ((start + cols.0).min(end), (start + cols.1).min(end));
        tab.doc.set_sels(vec![Sel::new(a, b)], 0);
        tab.view.scroll_to_caret = true;
    }

    /// Replace the matches in one open document as one undoable edit. Returns the count.
    fn replace_in_doc(&mut self, tab: usize, re: &regex::Regex, only_line: Option<usize>) -> usize {
        let (rep, expand) = (self.fif.replacement.clone(), self.fif.q.regex);
        let doc = &mut self.tabs[tab].doc;
        let text = doc.rope.to_string();
        let reps = findfiles::replacements(re, &text, &rep, expand, only_line);
        if reps.is_empty() {
            return 0;
        }
        // keep the caret where it was, shifted by the replacements before it
        let head = doc.rope.char_to_byte(doc.primary_sel().head);
        let mut new_head = head as isize;
        for (a, b, r) in &reps {
            if *b <= head {
                new_head += r.len() as isize - (b - a) as isize;
            }
        }
        let edits = reps
            .iter()
            .map(|(a, b, r)| {
                let n = r.chars().count();
                Edit { start: doc.rope.byte_to_char(*a), end: doc.rope.byte_to_char(*b), text: r.clone(), anchor: n, head: n }
            })
            .collect();
        doc.apply(EditKind::Other, edits);
        let new_text = findfiles::apply(&text, &reps);
        let mut h = (new_head.max(0) as usize).min(new_text.len());
        while !new_text.is_char_boundary(h) {
            h -= 1;
        }
        let c = doc.rope.byte_to_char(h);
        doc.set_sels(vec![Sel::caret(c)], 0);
        reps.len()
    }

    /// Replace in the selected result's line (opening its file), then move to the next one.
    fn fif_replace_one(&mut self, ctx: &egui::Context) {
        let Some((fi, hi)) = self.fif.sel else { return };
        let Some((path, line)) = self.fif.results.get(fi).and_then(|f| f.hits.get(hi).map(|h| (f.path.clone(), h.line))) else {
            return;
        };
        let re = match findfiles::build_regex(&self.fif.q) {
            Ok(re) => re,
            Err(e) => return self.set_status(ctx, e),
        };
        self.fif_open(fi, hi);
        if self.tabs[self.active].doc.path.as_deref() != Some(path.as_path()) {
            return;
        }
        let n = self.replace_in_doc(self.active, &re, Some(line));
        self.tabs[self.active].view.scroll_to_caret = true;
        self.set_status(ctx, format!("Replaced {n} in {} (unsaved; Ctrl+Z undoes)", file_name(&path)));
        self.fif.resume = Some((path, line + 1));
        self.fif.dirty_at = Some(0.0);
    }

    /// Ask before Replace All: count how many result files are open and how many would be
    /// written to disk.
    fn fif_ask_replace_all(&mut self, ctx: &egui::Context) {
        if self.fif.search.is_some() {
            return self.set_status(ctx, "Wait for the search to finish");
        }
        if matches!(self.fif.done, Some((_, true))) {
            return self.set_status(ctx, "Too many matches to replace safely; narrow the search first");
        }
        let open = self.fif.results.iter().filter(|f| self.tabs.iter().any(|t| t.doc.path.as_ref() == Some(&f.path))).count();
        self.fif.confirm = Some((open, self.fif.results.len() - open));
    }

    fn fif_replace_all(&mut self, ctx: &egui::Context) {
        self.fif.confirm = None;
        let re = match findfiles::build_regex(&self.fif.q) {
            Ok(re) => re,
            Err(e) => return self.set_status(ctx, e),
        };
        let (rep, expand) = (self.fif.replacement.clone(), self.fif.q.regex);
        let paths: Vec<PathBuf> = self.fif.results.iter().map(|f| f.path.clone()).collect();
        let (mut count, mut in_tabs, mut on_disk, mut failed) = (0, 0, 0, vec![]);
        let mut undo = vec![];
        for p in paths {
            if let Some(t) = self.tabs.iter().position(|t| t.doc.path.as_ref() == Some(&p)) {
                let n = self.replace_in_doc(t, &re, None);
                count += n;
                in_tabs += (n > 0) as usize;
                continue;
            }
            // not open: rewrite the file (only valid UTF-8, so nothing gets mangled)
            let Ok(before) = std::fs::read(&p).map_err(|_| ()).and_then(|b| String::from_utf8(b).map_err(|_| ())) else {
                failed.push(file_name(&p));
                continue;
            };
            let reps = findfiles::replacements(&re, &before, &rep, expand, None);
            if reps.is_empty() {
                continue;
            }
            let after = findfiles::apply(&before, &reps);
            let tmp = p.with_extension("edit-replace.tmp");
            match std::fs::write(&tmp, &after).and_then(|_| std::fs::rename(&tmp, &p)) {
                Ok(()) => {
                    count += reps.len();
                    on_disk += 1;
                    undo.push((p, before, after));
                }
                Err(_) => {
                    let _ = std::fs::remove_file(&tmp);
                    failed.push(file_name(&p));
                }
            }
        }
        self.fif.undo = undo;
        let mut msg = format!("Replaced {count} matches: {on_disk} files saved, {in_tabs} open files edited (unsaved)");
        if !failed.is_empty() {
            msg += &format!("; could not change {}", failed.join(", "));
        }
        self.set_status(ctx, msg);
        self.fif.dirty_at = Some(0.0);
    }

    /// Put back the files Replace All wrote, unless they have changed since.
    fn fif_undo_replace(&mut self, ctx: &egui::Context) {
        let (mut restored, mut skipped) = (0, vec![]);
        for (p, before, after) in std::mem::take(&mut self.fif.undo) {
            let unchanged = std::fs::read_to_string(&p).is_ok_and(|now| now == after);
            if unchanged && std::fs::write(&p, &before).is_ok() {
                restored += 1;
            } else {
                skipped.push(file_name(&p));
            }
        }
        let mut msg = format!("Restored {restored} files");
        if !skipped.is_empty() {
            msg += &format!("; skipped {} (changed since the replace)", skipped.join(", "));
        }
        self.set_status(ctx, msg + ". Use Ctrl+Z in open tabs.");
        self.fif.dirty_at = Some(0.0);
    }

    fn fif_step(&mut self, forward: bool) {
        let r = &self.fif.results;
        if r.is_empty() {
            return;
        }
        let (mut fi, mut hi) = self.fif.sel.unwrap_or((0, 0));
        if forward {
            if hi + 1 < r[fi].hits.len() {
                hi += 1;
            } else if fi + 1 < r.len() {
                (fi, hi) = (fi + 1, 0);
            }
        } else if hi > 0 {
            hi -= 1;
        } else if fi > 0 {
            fi -= 1;
            hi = r[fi].hits.len() - 1;
        }
        self.fif.sel = Some((fi, hi));
        self.fif.scroll_to_sel = true;
    }

    fn fif_panel(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let qid = Id::new("fif_query");
        let mid = Id::new("fif_mask");
        let rid = Id::new("fif_replacement");
        let in_replace = ctx.memory(|m| m.has_focus(rid));
        if ctx.memory(|m| m.has_focus(qid) || m.has_focus(mid)) || in_replace {
            let (esc, enter, up, down) = ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::Escape),
                    i.consume_key(Modifiers::NONE, Key::Enter),
                    i.consume_key(Modifiers::NONE, Key::ArrowUp),
                    i.consume_key(Modifiers::NONE, Key::ArrowDown),
                )
            });
            if esc {
                self.fif.open = false;
                self.focus_editor(ctx);
                return;
            }
            if up || down {
                self.fif_step(down);
            }
            if enter && in_replace {
                self.fif_replace_one(ctx);
            } else if enter {
                if let Some((fi, hi)) = self.fif.sel {
                    self.fif_open(fi, hi);
                    self.focus_editor(ctx);
                }
            }
        }
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label("Find in Files:");
            let r = ui.add(
                TextEdit::singleline(&mut self.fif.q.text)
                    .id(qid)
                    .desired_width(300.0)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("↑↓ select   Enter: open   Esc: close"),
            );
            if self.fif.focus {
                r.request_focus();
                select_all_text(ctx, qid, &self.fif.q.text);
                self.fif.focus = false;
            }
            changed |= r.changed();
            changed |= ui.toggle_value(&mut self.fif.q.case, "Aa").on_hover_text("Match case").changed();
            changed |= ui.toggle_value(&mut self.fif.q.whole, "W").on_hover_text("Whole words").changed();
            changed |= ui.toggle_value(&mut self.fif.q.regex, ".*").on_hover_text("Regular expression").changed();
            ui.label("Files:");
            changed |= ui
                .add(TextEdit::singleline(&mut self.fif.q.mask).id(mid).desired_width(130.0).hint_text("*.rs, !*.lock"))
                .on_hover_text("File name masks, comma separated; ! excludes")
                .changed();
            let status = if let Some(e) = &self.fif.error {
                RichText::new(e).color(Color32::from_rgb(0xe0, 0x6c, 0x75))
            } else if self.fif.q.text.is_empty() {
                RichText::new(if self.roots.is_empty() { "Searches open files (add a folder to search it)" } else { "" })
            } else {
                let (n, files) = (self.fif.lines, self.fif.results.len());
                let found = format!("{n} line{} in {files} file{}", if n == 1 { "" } else { "s" }, if files == 1 { "" } else { "s" });
                RichText::new(match self.fif.done {
                    None => format!("Searching… {found}"),
                    Some((_, true)) => format!("{found} (stopped at {} lines)", findfiles::MAX_LINES),
                    Some((searched, false)) if n == 0 => format!("No matches in {searched} files"),
                    Some(_) => found,
                })
                .weak()
            };
            ui.label(status);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("×").on_hover_text("Close (Esc)").clicked() {
                    self.fif.open = false;
                }
                ui.toggle_value(&mut self.fif.replace, "Replace…").on_hover_text("Replace in Files (Ctrl+Shift+R)");
            });
        });
        if changed {
            self.fif.dirty_at = Some(ctx.input(|i| i.time));
            self.fif.confirm = None;
        }
        if self.fif.replace {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.label("Replace with:");
                ui.add(
                    TextEdit::singleline(&mut self.fif.replacement)
                        .id(rid)
                        .desired_width(300.0)
                        .font(egui::TextStyle::Monospace)
                        .hint_text(if self.fif.q.regex { "$1 inserts group 1   Enter: replace" } else { "Enter: replace selected line" }),
                );
                let ready = self.fif.sel.is_some() && self.fif.error.is_none();
                if ui.add_enabled(ready, egui::Button::new("Replace")).on_hover_text("Replace in the selected line, then go to the next (Enter)").clicked() {
                    self.fif_replace_one(ctx);
                }
                if ui.add_enabled(ready, egui::Button::new("Replace All…")).clicked() {
                    self.fif_ask_replace_all(ctx);
                }
                if !self.fif.undo.is_empty()
                    && ui.button("Undo Replace").on_hover_text("Restore the files Replace All saved").clicked()
                {
                    self.fif_undo_replace(ctx);
                }
            });
            if let Some((open, disk)) = self.fif.confirm {
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    let mut msg = format!("Replace all matches in {} files?", open + disk);
                    if disk > 0 {
                        msg += &format!(" {disk} not open in the editor will be saved to disk (Undo Replace restores them).");
                    }
                    if open > 0 {
                        msg += &format!(" {open} open files are edited without saving.");
                    }
                    ui.label(RichText::new(msg).color(c_warn()));
                    if ui.button("Replace All").clicked() {
                        self.fif_replace_all(ctx);
                    }
                    if ui.button("Cancel").clicked() {
                        self.fif.confirm = None;
                    }
                });
            }
        }
        ui.add_space(4.0);

        // results: a flat list of rows (file headers and matching lines), drawn lazily
        let rows: Vec<(usize, Option<usize>)> = self
            .fif
            .results
            .iter()
            .enumerate()
            .flat_map(|(fi, f)| std::iter::once((fi, None)).chain((0..f.hits.len()).map(move |hi| (fi, Some(hi)))))
            .collect();
        let mono = FontId::monospace(self.settings.font_size - 1.0);
        let row_h = ui.fonts(|f| f.row_height(&mono)) + 4.0;
        let c = self.colors.clone();
        let sel_row = self.fif.sel.and_then(|s| rows.iter().position(|&(fi, hi)| (fi, hi) == (s.0, Some(s.1))));
        let mut scroll = egui::ScrollArea::vertical().id_salt("fif_results").auto_shrink([false, false]);
        if std::mem::take(&mut self.fif.scroll_to_sel) {
            if let Some(i) = sel_row {
                // keep the selected row inside the visible part of the list
                let step = row_h + ui.spacing().item_spacing.y;
                let (top, view_h) = (i as f32 * step, ui.available_height());
                let sid = ui.make_persistent_id("fif_results");
                let offset = egui::scroll_area::State::load(ctx, sid).map_or(0.0, |s| s.offset.y);
                if top < offset {
                    scroll = scroll.vertical_scroll_offset(top);
                } else if top + step > offset + view_h {
                    scroll = scroll.vertical_scroll_offset(top + step - view_h);
                }
            }
        }
        let mut clicked = None;
        scroll.show_rows(ui, row_h, rows.len(), |ui, range| {
            for i in range {
                let (fi, hi) = rows[i];
                let f = &self.fif.results[fi];
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                let p = ui.painter();
                match hi {
                    None => {
                        let name = f.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        let dir = self.rel(&f.path);
                        let dir = dir.strip_suffix(&name).unwrap_or(&dir).trim_end_matches('/');
                        let mut job = LayoutJob::default();
                        let ui_font = FontId::proportional(self.settings.font_size - 1.0);
                        job.append(&name, 0.0, TextFormat { font_id: ui_font.clone(), color: c.fg, ..Default::default() });
                        job.append(&format!("  {dir}"), 0.0, TextFormat { font_id: ui_font.clone(), color: c.gutter_fg, ..Default::default() });
                        job.append(&format!("  {}", f.hits.len()), 0.0, TextFormat { font_id: ui_font, color: c.gutter_fg, ..Default::default() });
                        let g = ui.fonts(|fo| fo.layout_job(job));
                        p.galley(egui::pos2(rect.left() + 4.0, rect.center().y - g.size().y / 2.0), g, c.fg);
                    }
                    Some(hi) => {
                        let h = &f.hits[hi];
                        if Some(i) == sel_row {
                            p.rect_filled(rect, 3.0, c.selection);
                        } else if resp.hovered() {
                            p.rect_filled(rect, 3.0, c.line_hl);
                        }
                        p.text(
                            egui::pos2(rect.left() + 54.0, rect.center().y),
                            Align2::RIGHT_CENTER,
                            (h.line + 1).to_string(),
                            mono.clone(),
                            c.gutter_fg,
                        );
                        let mut job = LayoutJob::default();
                        let mut at = 0;
                        let plain = TextFormat { font_id: mono.clone(), color: c.fg, ..Default::default() };
                        let hit = TextFormat { background: c.find, ..plain.clone() };
                        for &(s, e) in &h.ranges {
                            job.append(&h.preview[at..s], 0.0, plain.clone());
                            job.append(&h.preview[s..e], 0.0, hit.clone());
                            at = e;
                        }
                        job.append(&h.preview[at..], 0.0, plain);
                        let g = ui.fonts(|fo| fo.layout_job(job));
                        let clip = rect.intersect(ui.clip_rect());
                        ui.painter().with_clip_rect(clip).galley(
                            egui::pos2(rect.left() + 66.0, rect.center().y - g.size().y / 2.0),
                            g,
                            c.fg,
                        );
                        if resp.clicked() {
                            clicked = Some((fi, hi));
                        }
                    }
                }
            }
        });
        if let Some((fi, hi)) = clicked {
            self.fif_open(fi, hi);
            self.focus_editor(ctx);
        }
    }

    // ------------------------------------------------------------ external changes

    /// Follow changes on disk: refresh the tree and reload open files that changed.
    fn sync_fs(&mut self, ctx: &egui::Context) {
        let mut want = HashMap::new();
        for r in &self.roots {
            want.insert(r.clone(), watch::Mode::Recursive);
        }
        for t in &self.tabs {
            if let Some(dir) = t.doc.path.as_ref().and_then(|p| p.parent()) {
                if !self.roots.iter().any(|r| dir.starts_with(r)) && dir.is_dir() {
                    want.insert(dir.to_path_buf(), watch::Mode::NonRecursive);
                }
            }
        }
        self.fs.sync(&want);
        let (changes, rescan) = self.fs.drain();
        let now = ctx.input(|i| i.time);
        let mut touched: HashSet<PathBuf> = HashSet::new();
        for c in changes {
            if c.structural {
                if let Some(parent) = c.path.parent() {
                    self.dir_cache.remove(parent);
                }
                let skipped = c.path.components().any(|p| SKIP_DIRS.iter().any(|s| p.as_os_str() == *s));
                if !skipped {
                    self.files_stale = Some(now);
                }
            }
            self.dir_cache.remove(&c.path);
            touched.insert(c.path);
        }
        if rescan {
            self.dir_cache.clear();
            self.files_stale = Some(now);
        }
        for i in 0..self.tabs.len() {
            if self.tabs[i].doc.path.as_ref().is_some_and(|p| rescan || touched.contains(p)) {
                self.reload_from_disk(ctx, i);
            }
        }
        if let Some(t) = self.files_stale {
            if now - t >= 0.5 {
                self.files = walk_roots(&self.roots);
                self.files_stale = None;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(0.55 - (now - t)));
            }
        }
    }

    fn reload_from_disk(&mut self, ctx: &egui::Context, i: usize) {
        let doc = &mut self.tabs[i].doc;
        let Some(p) = doc.path.clone() else { return };
        let Ok(bytes) = std::fs::read(&p) else { return }; // deleted, or locked mid-write
        let text = String::from_utf8_lossy(&bytes);
        let disk = text.strip_prefix('\u{feff}').unwrap_or(&text).replace("\r\n", "\n");
        if doc.rope == disk.as_str() {
            return; // unchanged, e.g. our own save
        }
        let name = doc.name();
        if doc.is_dirty() {
            self.set_status(ctx, format!("{name} changed on disk; your unsaved edits are kept (Ctrl+D compares)"));
        } else {
            doc.reload(&text);
            if i == self.active {
                self.set_status(ctx, format!("Reloaded {name}: changed on disk"));
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
                let selected = self.tree_sel.contains(&path);
                let mut text = RichText::new(&name);
                if selected && self.tree_sel.len() > 1 {
                    text = text.strong().underline();
                }
                let r = ui.add(egui::SelectableLabel::new(selected || (is_active && self.tree_sel.len() <= 1), text));
                if r.clicked() {
                    if ui.input(|i| i.modifiers.ctrl || i.modifiers.command) {
                        // Ctrl+click: multi-select (like IntelliJ's Project view)
                        if let Some(i) = self.tree_sel.iter().position(|p| p == &path) {
                            if self.tree_sel.len() > 1 {
                                self.tree_sel.remove(i);
                            }
                        } else {
                            self.tree_sel.push(path.clone());
                        }
                        if self.tree_sel.len() == 2 {
                            self.status = Some(("2 files selected — right-click › Compare Selected Files".into(), 0.0));
                        }
                    } else {
                        self.tree_sel = vec![path.clone()];
                        self.to_open = Some(path.clone());
                    }
                }
                let r = r.on_hover_text(path.to_string_lossy());
                r.context_menu(|ui| {
                    // right-clicking outside the selection acts on that file alone
                    if !self.tree_sel.contains(&path) {
                        self.tree_sel = vec![path.clone()];
                    }
                    let pair = self.tree_pair();
                    let label = match &pair {
                        Some((a, b)) => format!("Compare Selected Files ({} ↔ {})", file_name(a), file_name(b)),
                        None => "Compare Selected Files (Ctrl+click two files)".into(),
                    };
                    if ui.add_enabled(pair.is_some(), egui::Button::new(label)).clicked() {
                        self.tree_action = pair.map(|(a, b)| TreeAction::Compare(a, b));
                        ui.close_menu();
                    }
                    let can_active = active.is_some_and(|a| a != path.as_path());
                    if ui.add_enabled(can_active, egui::Button::new("Compare with Active File")).clicked() {
                        self.tree_action = Some(TreeAction::CompareWithActive(path.clone()));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Open").clicked() {
                        self.to_open = Some(path.clone());
                        ui.close_menu();
                    }
                });
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
            // Hot exit: unsaved buffers are backed up and restored next time, so quitting
            // doesn't ask. Only if the backup fails do we fall back to the save prompt.
            let saved = self.persist_session(ctx, true);
            if !saved && !self.allow_quit && self.tabs.iter().any(|t| t.doc.is_dirty()) {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.pending = Some(Pending::Quit);
            }
        }

        self.sync_fs(ctx);
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
                    for cmd in [Cmd::NewFile, Cmd::OpenFile, Cmd::OpenFolder, Cmd::AddFolder, Cmd::Save, Cmd::SaveAs, Cmd::SaveAll, Cmd::CloseTab, Cmd::Quit] {
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
                    for cmd in [Cmd::Find, Cmd::FindInFiles, Cmd::ReplaceInFiles, Cmd::Replace, Cmd::FindNext, Cmd::FindPrev, Cmd::GotoAnything, Cmd::GotoLine, Cmd::GotoBracket] {
                        item(ui, cmd, self);
                    }
                });
                ui.menu_button("Diff", |ui| {
                    for cmd in [Cmd::DiffWithSaved, Cmd::DiffWithHead, Cmd::DiffWithClipboard, Cmd::DiffWithFile, Cmd::DiffWithTab, Cmd::DiffTreeSelection, Cmd::ShowDiff, Cmd::NextDiff, Cmd::PrevDiff, Cmd::CloseDiff] {
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
                    if !self.roots.is_empty() {
                        let active = self.tabs[self.active].doc.path.clone();
                        // Without a multi-selection, the tree's selection follows the active
                        // file, so Ctrl+clicking one more file selects a pair (as in IntelliJ).
                        if self.tree_followed != active {
                            self.tree_followed = active.clone();
                            if self.tree_sel.len() <= 1 {
                                self.tree_sel = active.iter().cloned().collect();
                            }
                        }
                        let mut remove = None;
                        for (i, root) in self.roots.clone().into_iter().enumerate() {
                            let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
                            let id = ui.make_persistent_id(("root", &root));
                            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true)
                                .show_header(ui, |ui| {
                                    let r = ui.label(RichText::new(name.to_uppercase()).small().strong()).on_hover_text(root.display().to_string());
                                    r.context_menu(|ui| {
                                        if ui.button("Remove Folder from Sidebar").clicked() {
                                            remove = Some(i);
                                            ui.close_menu();
                                        }
                                    });
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui.small_button("×").on_hover_text("Remove folder from sidebar").clicked() {
                                            remove = Some(i);
                                        }
                                        if ui.small_button("⟳").on_hover_text("Refresh").clicked() {
                                            self.dir_cache.retain(|p, _| !p.starts_with(&root));
                                            self.files_stale = Some(0.0);
                                        }
                                    });
                                })
                                .body(|ui| self.tree_ui(ui, &root, active.as_deref()));
                        }
                        if let Some(i) = remove {
                            self.remove_root(i);
                        }
                        ui.add_space(4.0);
                        if ui.small_button("+ Add Folder…").clicked() {
                            self.run_global(ctx, Cmd::AddFolder);
                        }
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
        if let Some(a) = self.tree_action.take() {
            self.run_tree_action(ctx, a);
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

        // ---------------- find in files
        self.fif_pump(ctx);
        if self.fif.open {
            egui::TopBottomPanel::bottom("find_in_files")
                .resizable(true)
                .default_height(260.0)
                .frame(Frame::none().fill(c.panel).inner_margin(Margin::symmetric(4.0, 6.0)))
                .show(ctx, |ui| self.fif_panel(ctx, ui));
        }

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
            let tab = &mut self.tabs[self.active];
            if let Some(d) = tab.diff.as_mut() {
                let cx = diffview::Ctx {
                    syn: &self.syn,
                    theme,
                    colors: &self.colors,
                    settings: &self.settings,
                    find,
                    right_id: self.editor_id,
                    left_id: self.diff_left_id,
                };
                if let diffview::Outcome::Close = diffview::show(ui, &mut tab.doc, &mut tab.view, d, &cx) {
                    tab.diff = None;
                    ui.memory_mut(|m| m.request_focus(self.editor_id));
                }
            } else {
                let cx = editor::Ctx {
                    syn: &self.syn,
                    theme,
                    colors: &self.colors,
                    settings: &self.settings,
                    find,
                    id: self.editor_id,
                    decor: None,
                };
                let resp = editor::show(ui, &mut tab.doc, &mut tab.view, &cx);
                if std::mem::take(&mut self.startup_focus) {
                    resp.request_focus();
                }
            }
        });

        self.palette_ui(ctx);
        self.dialogs(ctx);

        // ---------------- window title
        let d = &self.tabs[self.active].doc;
        let title = format!(
            "{}{}{} — edit{}",
            if d.is_dirty() { "● " } else { "" },
            d.name(),
            match self.roots.as_slice() {
                [] => String::new(),
                [r, rest @ ..] => {
                    let name = r.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if rest.is_empty() { format!(" · {name}") } else { format!(" · {name} +{}", rest.len()) }
                }
            },
            option_env!("EDIT_VERSION").map(|v| format!(" {v}")).unwrap_or_default()
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

/// Files under all sidebar folders (for Go to File), without duplicates from nested folders.
fn walk_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = vec![];
    for (i, r) in roots.iter().enumerate() {
        // a folder inside another sidebar folder is already covered by the outer one
        if roots.iter().enumerate().any(|(j, o)| j != i && r.starts_with(o) && (r != o || j < i)) {
            continue;
        }
        out.extend(walk(r, 50_000usize.saturating_sub(out.len())));
    }
    out
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
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates.push(home.join("Library/Fonts/JetBrainsMono-Regular.ttf"));
        candidates.push(home.join(".local/share/fonts/JetBrainsMono-Regular.ttf"));
    }
    for p in [
        "/Library/Fonts/JetBrainsMono-Regular.ttf",
        "/System/Library/Fonts/SFNSMono.ttf",
        "/System/Library/Fonts/Menlo.ttc",
        "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
        "/usr/share/fonts/truetype/ubuntu/UbuntuMono-R.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    ] {
        candidates.push(PathBuf::from(p));
    }
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

/// Contents of `path` at Git HEAD (`git show HEAD:./name`, run from the file's folder).
fn git_head_text(path: &Path) -> Result<String, String> {
    let dir = path.parent().ok_or("File has no folder")?;
    let name = path.file_name().ok_or("Not a file")?.to_string_lossy().into_owned();
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(dir).arg("show").arg(format!("HEAD:./{name}"));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flash
    }
    let out = cmd.output().map_err(|e| format!("Could not run git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git: {}", err.lines().next().unwrap_or("file is not in HEAD").trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Warning text colour (amber).
fn c_warn() -> Color32 {
    Color32::from_rgb(0xeb, 0xcb, 0x8b)
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}
