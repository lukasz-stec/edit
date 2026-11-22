//! Incremental syntax highlighting with syntect (Sublime .sublime-syntax / .tmTheme).

use eframe::egui::Color32;
use ropey::Rope;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use syntect::highlighting::{
    Color, HighlightIterator, HighlightState, Highlighter, Theme, ThemeSet,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};

pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .map(|d| PathBuf::from(d).join("edit"))
}

pub struct Syntax {
    pub ss: SyntaxSet,
    pub themes: ThemeSet,
}

impl Syntax {
    /// Built-in syntaxes/themes plus any found in %APPDATA%/edit/{syntaxes,themes}.
    pub fn load() -> Self {
        let mut ss = SyntaxSet::load_defaults_newlines();
        let mut themes = ThemeSet::load_defaults();
        if let Some(dir) = config_dir() {
            let sd = dir.join("syntaxes");
            if sd.is_dir() {
                let mut b = ss.into_builder();
                let _ = b.add_from_folder(&sd, true);
                ss = b.build();
            }
            let td = dir.join("themes");
            if td.is_dir() {
                let _ = themes.add_from_folder(&td);
            }
        }
        Self { ss, themes }
    }

    pub fn detect(&self, path: Option<&Path>, first_line: &str) -> String {
        let by_ext = path.and_then(|p| {
            let name = p.file_name()?.to_string_lossy().to_string();
            let ext = p.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or(name);
            self.ss.find_syntax_by_extension(&ext)
        });
        by_ext
            .or_else(|| self.ss.find_syntax_by_first_line(first_line))
            .unwrap_or_else(|| self.ss.find_syntax_plain_text())
            .name
            .clone()
    }

    pub fn theme(&self, name: &str) -> &Theme {
        self.themes
            .themes
            .get(name)
            .or_else(|| self.themes.themes.values().next())
            .expect("no themes")
    }
}

pub fn color(c: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

/// Per-document cache: parser state at the start of every line + colored spans per line.
pub struct LineCache {
    pub syntax: String,
    states: Vec<(ParseState, HighlightState)>,
    /// (char count, color) runs per line
    pub spans: Vec<Vec<(usize, Color32)>>,
}

impl LineCache {
    pub fn new(syntax: String) -> Self {
        Self { syntax, states: vec![], spans: vec![] }
    }

    pub fn invalidate(&mut self, line: usize) {
        self.spans.truncate(line);
        self.states.truncate(line + 1);
    }

    pub fn reset(&mut self) {
        self.spans.clear();
        self.states.clear();
    }

    /// Highlight lines up to `upto` within `budget`. Returns true when done.
    pub fn ensure(&mut self, rope: &Rope, upto: usize, syn: &Syntax, theme: &Theme, budget: Duration) -> bool {
        let Some(syntax) = syn.ss.find_syntax_by_name(&self.syntax) else {
            return true;
        };
        let hl = Highlighter::new(theme);
        if self.states.is_empty() {
            self.spans.clear();
            self.states.push((ParseState::new(syntax), HighlightState::new(&hl, ScopeStack::new())));
        }
        let upto = upto.min(rope.len_lines());
        let fg = color(theme.settings.foreground.unwrap_or(Color::WHITE));
        let t0 = Instant::now();
        while self.spans.len() < upto {
            let i = self.spans.len();
            let (mut ps, mut hs) = self.states[i].clone();
            let slice = rope.line(i);
            let n = slice.len_chars();
            let mut spans = Vec::new();
            if n > 5000 {
                // Pathologically long line (minified file): don't parse it.
                spans.push((n, fg));
            } else {
                let mut line = slice.to_string();
                if !line.ends_with('\n') {
                    line.push('\n');
                }
                match ps.parse_line(&line, &syn.ss) {
                    Ok(ops) => {
                        for (style, text) in HighlightIterator::new(&mut hs, &ops, &line, &hl) {
                            spans.push((text.chars().count(), color(style.foreground)));
                        }
                    }
                    Err(_) => spans.push((n, fg)),
                }
            }
            self.spans.push(spans);
            self.states.push((ps, hs));
            if t0.elapsed() > budget {
                return false;
            }
        }
        true
    }
}

/// Editor colors derived from the active theme.
#[derive(Clone)]
pub struct Colors {
    pub bg: Color32,
    pub fg: Color32,
    pub caret: Color32,
    pub selection: Color32,
    pub line_hl: Color32,
    pub gutter_bg: Color32,
    pub gutter_fg: Color32,
    pub gutter_fg_active: Color32,
    pub guide: Color32,
    pub find: Color32,
    pub occurrence: Color32,
    pub bracket: Color32,
    pub panel: Color32,
    pub dark: bool,
}

pub fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

impl Colors {
    pub fn from_theme(t: &Theme) -> Self {
        let s = &t.settings;
        let bg = s.background.map(color).unwrap_or(Color32::from_rgb(40, 44, 52));
        let bg = Color32::from_rgb(bg.r(), bg.g(), bg.b());
        let fg = s.foreground.map(color).unwrap_or(Color32::from_gray(220));
        let lum = 0.299 * bg.r() as f32 + 0.587 * bg.g() as f32 + 0.114 * bg.b() as f32;
        let dark = lum < 128.0;
        let selection = s
            .selection
            .map(color)
            .map(|c| if c.a() == 255 { c } else { lerp(bg, Color32::from_rgb(c.r(), c.g(), c.b()), c.a() as f32 / 255.0) })
            .unwrap_or(lerp(bg, Color32::from_rgb(90, 120, 200), 0.45));
        Colors {
            bg,
            fg,
            caret: s.caret.map(color).unwrap_or(fg),
            selection,
            line_hl: s.line_highlight.map(color).unwrap_or(lerp(bg, fg, 0.06)),
            gutter_bg: bg,
            gutter_fg: lerp(bg, fg, 0.35),
            gutter_fg_active: lerp(bg, fg, 0.85),
            guide: lerp(bg, fg, 0.12),
            find: Color32::from_rgba_unmultiplied(230, 180, 40, 90),
            occurrence: lerp(bg, fg, 0.35),
            bracket: lerp(bg, fg, 0.6),
            panel: if dark { lerp(bg, Color32::BLACK, 0.25) } else { lerp(bg, Color32::from_gray(200), 0.4) },
            dark,
        }
    }
}
