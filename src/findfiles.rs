//! Find in Files: searches the sidebar folders on a background thread, streaming results
//! back per file so the UI stays responsive on big trees.

use eframe::egui;
use regex::{Regex, RegexBuilder};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

/// Stop after this many matching lines, so a too-broad search stays usable.
pub const MAX_LINES: usize = 10_000;
const MAX_FILE: u64 = 20 << 20;
const PREVIEW: usize = 240;

#[derive(Clone, PartialEq, Default)]
pub struct Query {
    pub text: String,
    pub case: bool,
    pub whole: bool,
    pub regex: bool,
    /// File name masks, e.g. `*.rs, *.toml`; `!` excludes (`!*.lock`). Empty = all files.
    pub mask: String,
}

/// One line containing matches.
pub struct Hit {
    /// 0-based line number.
    pub line: usize,
    /// Char columns of the first match in that line (where a click jumps to).
    pub cols: (usize, usize),
    /// The line, trimmed and shortened for display.
    pub preview: String,
    /// Byte ranges of all matches inside `preview`.
    pub ranges: Vec<(usize, usize)>,
}

pub struct FileHits {
    pub path: PathBuf,
    pub hits: Vec<Hit>,
}

pub enum Msg {
    File(FileHits),
    /// Search finished: number of files searched, and whether it stopped at MAX_LINES.
    Done(usize, bool),
}

pub struct Search {
    pub rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Search {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn build_regex(q: &Query) -> Result<Regex, String> {
    let pat = if q.regex { q.text.clone() } else { regex::escape(&q.text) };
    let pat = if q.whole { format!(r"\b(?:{pat})\b") } else { pat };
    RegexBuilder::new(&pat).case_insensitive(!q.case).build().map_err(|e| {
        // the regex crate's messages are multi-line with an ASCII-art pointer
        e.to_string().lines().last().unwrap_or("invalid regular expression").trim().to_string()
    })
}

/// `*` and `?` wildcard match, case-insensitive.
fn wildcard(pat: &[char], name: &[char]) -> bool {
    match (pat.first(), name.first()) {
        (None, None) => true,
        (Some('*'), _) => wildcard(&pat[1..], name) || (!name.is_empty() && wildcard(pat, &name[1..])),
        (Some('?'), Some(_)) => wildcard(&pat[1..], &name[1..]),
        (Some(p), Some(n)) => p.to_lowercase().eq(n.to_lowercase()) && wildcard(&pat[1..], &name[1..]),
        _ => false,
    }
}

pub struct Mask {
    include: Vec<Vec<char>>,
    exclude: Vec<Vec<char>>,
}

impl Mask {
    pub fn new(s: &str) -> Self {
        let (mut include, mut exclude) = (vec![], vec![]);
        for part in s.split([',', ';', ' ']).map(str::trim).filter(|p| !p.is_empty()) {
            match part.strip_prefix('!') {
                Some(p) => exclude.push(p.chars().collect()),
                None => include.push(part.chars().collect()),
            }
        }
        Self { include, exclude }
    }

    pub fn matches(&self, name: &str) -> bool {
        let n: Vec<char> = name.chars().collect();
        (self.include.is_empty() || self.include.iter().any(|p| wildcard(p, &n)))
            && !self.exclude.iter().any(|p| wildcard(p, &n))
    }
}

/// Matching lines of one file's text.
pub fn search_text(re: &Regex, text: &str, budget: usize) -> Vec<Hit> {
    let mut hits = vec![];
    for (no, line) in text.split('\n').enumerate() {
        if hits.len() >= budget {
            break;
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        let found: Vec<(usize, usize)> =
            re.find_iter(line).filter(|m| m.end() > m.start()).map(|m| (m.start(), m.end())).collect();
        let Some(&(s0, e0)) = found.first() else { continue };
        // preview: skip indentation, and start near the first match on very long lines
        let indent = line.len() - line.trim_start().len();
        let mut from = indent.min(s0);
        if s0 - from > PREVIEW / 2 {
            from = s0 - PREVIEW / 3;
            while !line.is_char_boundary(from) {
                from += 1;
            }
        }
        let mut to = (from + PREVIEW).min(line.len());
        while !line.is_char_boundary(to) {
            to -= 1;
        }
        let ranges = found
            .iter()
            .filter(|&&(s, e)| s < to && e > from)
            .map(|&(s, e)| (s.max(from) - from, e.min(to) - from))
            .collect();
        let col = |b: usize| line[..b].chars().count();
        hits.push(Hit { line: no, cols: (col(s0), col(e0)), preview: line[from..to].to_string(), ranges });
    }
    hits
}

/// Start searching `files` (open buffers' text in `buffers` wins over the disk copy).
pub fn start(
    files: Vec<PathBuf>,
    buffers: HashMap<PathBuf, String>,
    q: &Query,
    ctx: egui::Context,
) -> Result<Search, String> {
    let re = build_regex(q)?;
    let mask = Mask::new(&q.mask);
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let stop = cancel.clone();
    std::thread::spawn(move || {
        let (mut total, mut searched) = (0, 0);
        let mut last_wake = std::time::Instant::now();
        for path in files {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
            if !mask.matches(&name) {
                continue;
            }
            let text = match buffers.get(&path) {
                Some(t) => t.clone(),
                None => {
                    if std::fs::metadata(&path).map_or(true, |m| m.len() > MAX_FILE) {
                        continue;
                    }
                    let Ok(bytes) = std::fs::read(&path) else { continue };
                    if bytes[..bytes.len().min(8000)].contains(&0) {
                        continue; // binary
                    }
                    String::from_utf8_lossy(&bytes).into_owned()
                }
            };
            searched += 1;
            let hits = search_text(&re, &text, MAX_LINES - total);
            if !hits.is_empty() {
                total += hits.len();
                if tx.send(Msg::File(FileHits { path, hits })).is_err() {
                    return;
                }
                if last_wake.elapsed().as_millis() > 50 {
                    ctx.request_repaint();
                    last_wake = std::time::Instant::now();
                }
                if total >= MAX_LINES {
                    let _ = tx.send(Msg::Done(searched, true));
                    ctx.request_repaint();
                    return;
                }
            }
        }
        let _ = tx.send(Msg::Done(searched, false));
        ctx.request_repaint();
    });
    Ok(Search { rx, cancel })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks() {
        let m = Mask::new("*.rs, *.TOML !main.rs");
        assert!(m.matches("lib.rs") && m.matches("Cargo.toml"));
        assert!(!m.matches("main.rs") && !m.matches("notes.md"));
        assert!(Mask::new("").matches("anything"));
        assert!(Mask::new("!*.lock").matches("a.rs") && !Mask::new("!*.lock").matches("Cargo.lock"));
    }

    #[test]
    fn hits_per_line_with_columns() {
        let q = Query { text: "foo".into(), ..Default::default() };
        let re = build_regex(&q).unwrap();
        let hits = search_text(&re, "a\r\n    let fóo = Foo + foo;\nnone\n", 100);
        assert_eq!(hits.len(), 1);
        let h = &hits[0];
        assert_eq!(h.line, 1);
        assert_eq!(h.cols, (14, 17)); // first match "Foo" (case-insensitive), in chars
        assert_eq!(h.preview, "let fóo = Foo + foo;");
        assert_eq!(h.ranges.len(), 2);
        assert_eq!(&h.preview[h.ranges[1].0..h.ranges[1].1], "foo");
    }

    #[test]
    fn whole_word_and_bad_regex() {
        let q = Query { text: "id".into(), whole: true, ..Default::default() };
        assert_eq!(search_text(&build_regex(&q).unwrap(), "idx id\n", 10)[0].cols, (4, 6));
        let bad = Query { text: "(".into(), regex: true, ..Default::default() };
        assert!(build_regex(&bad).is_err());
    }
}
