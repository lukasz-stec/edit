//! Incremental syntax highlighting with syntect (Sublime .sublime-syntax / .tmTheme).

use eframe::egui::Color32;
use ropey::Rope;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use syntect::highlighting::{
    Color, HighlightIterator, HighlightState, Highlighter, Theme, ThemeSet,
};
use syntect::parsing::{ParseState, Scope, ScopeStack, ScopeStackOp, SyntaxSet};

/// Settings folder: `%APPDATA%\edit` on Windows, `~/Library/Application Support/edit` on
/// macOS, `$XDG_CONFIG_HOME/edit` or `~/.config/edit` elsewhere.
pub fn config_dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        env("APPDATA")
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library").join("Application Support"))
    } else {
        env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config")))
    };
    base.map(|d| d.join("edit"))
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
    /// (char count, prose?) runs per line: the text spell checking looks at.
    pub prose: Vec<Vec<(usize, bool)>>,
}

/// Which text is prose: in text formats everything but code and links, in code only
/// comments and strings.
struct ProseScopes {
    text: bool,
    include: Vec<Scope>,
    exclude: Vec<Scope>,
}

impl ProseScopes {
    fn new(syntax: &str) -> Self {
        let s = |names: &[&str]| names.iter().filter_map(|n| Scope::new(n).ok()).collect::<Vec<_>>();
        let text = matches!(syntax, "Plain Text" | "Markdown" | "MultiMarkdown" | "Git Commit" | "Git Commit Message")
            || syntax.contains("Markdown");
        Self {
            text,
            include: s(&["comment", "string"]),
            exclude: s(&[
                "markup.raw",
                "meta.code-fence",
                "markup.underline.link",
                "meta.link.reference",
                "constant.other.reference.link",
                "meta.tag",
                "string.regexp",
                "meta.interpolation",
                "constant.character.escape",
                "source",
            ]),
        }
    }

    fn is_prose(&self, stack: &ScopeStack) -> bool {
        let scopes = stack.as_slice();
        let has = |set: &[Scope]| scopes.iter().any(|s| set.iter().any(|p| p.is_prefix_of(*s)));
        if self.text {
            // "source.*" only excludes code embedded in a text format (fenced blocks)
            !has(&self.exclude)
        } else {
            has(&self.include) && !has(&self.exclude[..self.exclude.len() - 1])
        }
    }
}

/// Walk a line's scope changes, starting from the scopes open at its start.
fn prose_runs(line: &str, ops: &[(usize, ScopeStackOp)], mut stack: ScopeStack, scopes: &ProseScopes) -> Vec<(usize, bool)> {
    let mut runs: Vec<(usize, bool)> = vec![];
    let mut push = |text: &str, p: bool| {
        let n = text.chars().count();
        match runs.last_mut() {
            Some(last) if last.1 == p => last.0 += n,
            _ if n > 0 => runs.push((n, p)),
            _ => {}
        }
    };
    let mut pos = 0;
    for (off, op) in ops {
        let off = (*off).min(line.len());
        if off > pos {
            push(&line[pos..off], scopes.is_prose(&stack));
            pos = off;
        }
        let _ = stack.apply(op);
    }
    push(&line[pos..], scopes.is_prose(&stack));
    runs
}

impl LineCache {
    pub fn new(syntax: String) -> Self {
        Self { syntax, states: vec![], spans: vec![], prose: vec![] }
    }

    pub fn invalidate(&mut self, line: usize) {
        self.spans.truncate(line);
        self.prose.truncate(line);
        self.states.truncate(line + 1);
    }

    pub fn reset(&mut self) {
        self.spans.clear();
        self.prose.clear();
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
            self.prose.clear();
            self.states.push((ParseState::new(syntax), HighlightState::new(&hl, ScopeStack::new())));
        }
        let upto = upto.min(rope.len_lines());
        if self.spans.len() >= upto {
            return true;
        }
        let fg = color(theme.settings.foreground.unwrap_or(Color::WHITE));
        let scopes = ProseScopes::new(&self.syntax);
        let t0 = Instant::now();
        while self.spans.len() < upto {
            let i = self.spans.len();
            let (mut ps, mut hs) = self.states[i].clone();
            let slice = rope.line(i);
            let n = slice.len_chars();
            let mut spans = Vec::new();
            let mut prose = Vec::new();
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
                        prose = prose_runs(&line, &ops, hs.path.clone(), &scopes);
                        for (style, text) in HighlightIterator::new(&mut hs, &ops, &line, &hl) {
                            spans.push((text.chars().count(), color(style.foreground)));
                        }
                    }
                    Err(_) => spans.push((n, fg)),
                }
            }
            self.spans.push(spans);
            self.prose.push(prose);
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
    /// Wavy underline under misspelled words (IntelliJ's green "typo").
    pub typo: Color32,
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
            typo: if dark { Color32::from_rgb(0x6a, 0xab, 0x73) } else { Color32::from_rgb(0x3d, 0x8b, 0x4a) },
            panel: if dark { lerp(bg, Color32::BLACK, 0.25) } else { lerp(bg, Color32::from_gray(200), 0.4) },
            dark,
        }
    }
}
