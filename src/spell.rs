//! English spell checking (Hunspell en_US dictionary via spellbook).
//!
//! Plain text and Markdown are checked as a whole; in code only comments and strings
//! (see `LineCache::prose`). Words added by the user live in `<config>/dictionary.txt`.

use crate::highlight::config_dir;
use spellbook::Dictionary;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};

const AFF: &str = include_str!("../assets/dict/en_US.aff");
const DIC: &str = include_str!("../assets/dict/en_US.dic");

/// Words common in code comments and technical writing that en_US lacks.
const TECH_WORDS: &str = "api apis app apps arg args async auth backend backtick bool boolean booleans \
    builtin bytecode callback callbacks changelog checkbox checksum cli codebase config configs \
    const cpu css csv dataset deserialize dev dropdown enum enums env eol filename filenames \
    filesystem frontend func github gpu hashmap html http https impl init inline iterator \
    iterators json jsx linter localhost lookup macos metadata middleware mutex namespace namespaces \
    natively newline newlines npm param params parsers plugin plugins prepend repo repos runtime \
    sql stderr stdin stdout struct structs subfolder submodule symlink symlinks tooltip tooltips \
    toolbar tuple tuples typedef ui unescape unicode url urls usize utf vec walkthrough webhook \
    whitespace workflow workflows xml yaml";

fn user_dict_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("dictionary.txt"))
}

pub struct Speller {
    dict: Option<Dictionary>,
    rx: Option<Receiver<Dictionary>>,
    /// word → correct?
    cache: RefCell<HashMap<String, bool>>,
}

impl Speller {
    /// Loads the dictionary on a background thread; nothing is flagged until it is ready.
    pub fn new() -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let Ok(mut d) = Dictionary::new(AFF, DIC) else { return };
            for w in TECH_WORDS.split_whitespace() {
                let _ = d.add(w);
            }
            if let Some(text) = user_dict_path().and_then(|p| std::fs::read_to_string(p).ok()) {
                for w in text.lines().map(str::trim).filter(|w| !w.is_empty()) {
                    let _ = d.add(w);
                }
            }
            let _ = tx.send(d);
        });
        Self { dict: None, rx: Some(rx), cache: RefCell::new(HashMap::new()) }
    }

    /// Picks up the loaded dictionary. Returns true on the frame it becomes ready.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        match rx.try_recv() {
            Ok(d) => {
                self.dict = Some(d);
                self.rx = None;
                true
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.rx = None;
                false
            }
            Err(_) => false,
        }
    }

    pub fn loading(&self) -> bool {
        self.rx.is_some()
    }

    pub fn check(&self, word: &str) -> bool {
        let Some(d) = &self.dict else { return true };
        let mut cache = self.cache.borrow_mut();
        if let Some(&ok) = cache.get(word) {
            return ok;
        }
        if cache.len() > 50_000 {
            cache.clear();
        }
        let ok = d.check(&word.replace('’', "'"));
        cache.insert(word.to_string(), ok);
        ok
    }

    /// Misspelled words in a line: char ranges. `prose` is (char count, checked?) runs.
    pub fn typos(&self, line: &str, prose: &[(usize, bool)]) -> Vec<(usize, usize)> {
        if self.dict.is_none() {
            return vec![];
        }
        words(line, prose).into_iter().filter(|w| !self.check(&w.2)).map(|w| (w.0, w.1)).collect()
    }

    pub fn suggest(&self, word: &str) -> Vec<String> {
        let mut out = vec![];
        if let Some(d) = &self.dict {
            d.suggest(&word.replace('’', "'"), &mut out);
        }
        out.truncate(8);
        out
    }

    /// Accept a word from now on and remember it in the user dictionary.
    pub fn add_word(&mut self, word: &str) -> Result<(), String> {
        let word = word.trim();
        if let Some(d) = &mut self.dict {
            d.add(word).map_err(|e| e.to_string())?;
        }
        self.cache.borrow_mut().remove(word);
        let path = user_dict_path().ok_or("no config folder")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| e.to_string())?;
        writeln!(f, "{word}").map_err(|e| e.to_string())
    }
}

/// Characters that make a whitespace-separated chunk look like code, a path, an address or
/// markup rather than prose; such chunks are not checked.
fn codeish(chunk: &[char]) -> bool {
    let n = chunk.len();
    chunk.iter().enumerate().any(|(i, &c)| {
        let inner = i > 0 && i + 1 < n && chunk[i + 1].is_alphanumeric();
        match c {
            '/' | '\\' | '@' | '#' | '$' | '%' | '=' | '<' | '>' | '{' | '}' | '|' | '^' | '`' => true,
            // "std::io", "file.rs", "a.b" — but not "end." or "word:"
            ':' if chunk.get(i + 1) == Some(&':') => true,
            ':' | '.' => inner && chunk[i - 1].is_alphanumeric(),
            _ => false,
        }
    })
}

/// Words to check in a line: (start, end) char range and the word. Only `prose` runs are
/// looked at; skipped: code-like chunks, words with digits or `_`, ALLCAPS and camelCase.
pub fn words(line: &str, prose: &[(usize, bool)]) -> Vec<(usize, usize, String)> {
    let chars: Vec<char> = line.trim_end_matches(['\n', '\r']).chars().collect();
    // which chars are prose
    let mut ok = vec![false; chars.len()];
    let mut at = 0;
    for &(n, p) in prose {
        let end = (at + n).min(chars.len());
        if p {
            ok[at..end].iter_mut().for_each(|x| *x = true);
        }
        at = end;
    }
    let mut out = vec![];
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let cs = i;
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        let chunk = &chars[cs..i];
        if codeish(chunk) {
            continue;
        }
        // tokens inside the chunk: letters, digits, '_' and apostrophes
        let tok = |c: char| c.is_alphanumeric() || c == '_' || c == '\'' || c == '’';
        let mut j = cs;
        while j < i {
            if !tok(chars[j]) {
                j += 1;
                continue;
            }
            let ts = j;
            while j < i && tok(chars[j]) {
                j += 1;
            }
            // drop quotes and _emphasis_ around the token
            let (mut a, mut b) = (ts, j);
            while a < b && matches!(chars[a], '\'' | '’' | '_') {
                a += 1;
            }
            while b > a && matches!(chars[b - 1], '\'' | '’' | '_') {
                b -= 1;
            }
            let w = &chars[a..b];
            let letters = w.iter().filter(|c| c.is_alphabetic()).count();
            let skip = letters < 2
                || w.len() > 40
                || w.iter().any(|c| c.is_numeric() || *c == '_')
                || w.iter().all(|c| !c.is_lowercase())
                || w[1..].iter().any(|c| c.is_uppercase())
                || !ok[a..b].iter().all(|x| *x);
            if !skip {
                out.push((a, b, w.iter().collect()));
            }
        }
    }
    out
}

/// The checked word containing (or ending at) char `col`, if any.
pub fn word_at(line: &str, prose: &[(usize, bool)], col: usize) -> Option<(usize, usize, String)> {
    words(line, prose).into_iter().find(|w| w.0 <= col && col <= w.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(line: &str) -> Vec<String> {
        words(line, &[(usize::MAX, true)]).into_iter().map(|w| w.2).collect()
    }

    #[test]
    fn word_splitting() {
        assert_eq!(ws("Hello, world! It's fine."), ["Hello", "world", "It's", "fine"]);
        assert_eq!(ws("see https://x.org and file.rs or std::io"), ["see", "and", "or"]);
        assert_eq!(ws("camelCase snake_case HTTP v2 x 'quoted' well-known"), ["quoted", "well", "known"]);
        assert_eq!(ws("**bold** _em_ (paren) end. word:"), ["bold", "em", "paren", "end", "word"]);
        assert_eq!(ws("use `code` and <tag> or $var"), ["use", "and", "or"]);
        // only prose runs: "// teh" is a comment after 7 chars of code
        let line = "let x; // teh";
        let w: Vec<_> = words(line, &[(7, false), (6, true)]).into_iter().map(|w| (w.0, w.1, w.2)).collect();
        assert_eq!(w, vec![(10, 13, "teh".to_string())]);
        assert_eq!(word_at(line, &[(7, false), (6, true)], 13).map(|w| w.2), Some("teh".into()));
        assert_eq!(word_at(line, &[(7, false), (6, true)], 2), None);
    }

    #[test]
    fn dictionary_checks_and_suggests() {
        let mut s = Speller::new();
        let t0 = std::time::Instant::now();
        while !s.poll() {
            assert!(t0.elapsed().as_secs() < 30, "dictionary did not load");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(s.typos("This sentense has a speling mistake.", &[(usize::MAX, true)]), vec![(5, 13), (20, 27)]);
        assert!(s.check("Hello") && s.check("doesn't") && s.check("doesn’t") && s.check("config"));
        assert!(s.suggest("sentense").iter().any(|w| w == "sentence"));
    }
}
