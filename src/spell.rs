//! English spell checking (Hunspell en_US dictionary via spellbook).
//!
//! Plain text and Markdown are checked as a whole; in code only comments and strings
//! (see `LineCache::prose`). Words added by the user live in `<config>/dictionary.txt`,
//! one per line (`#` starts a comment); the file is watched and reloaded when it changes.

use crate::highlight::config_dir;
use spellbook::Dictionary;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Instant, SystemTime};

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

const HEADER: &str = "# Your spelling dictionary: one word per line. Lines starting with # are ignored.\n\
                      # Saved changes apply right away; delete a line to remove a word.\n";

/// `<config>/dictionary.txt`
pub fn user_dict_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("dictionary.txt"))
}

/// The words in a user dictionary file.
pub fn parse_user(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|w| !w.is_empty() && !w.starts_with('#')).map(String::from).collect()
}

/// The file without `word` (comments and other lines kept as they are).
pub fn remove_line(text: &str, word: &str) -> String {
    text.lines().filter(|l| l.trim() != word).map(|l| format!("{l}\n")).collect()
}

fn mtime(p: &Option<PathBuf>) -> Option<SystemTime> {
    p.as_ref().and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok())
}

pub struct Speller {
    dict: Option<Dictionary>,
    /// A (re)load in progress; the old dictionary stays in use until it arrives.
    rx: Option<Receiver<(Dictionary, HashSet<String>)>>,
    /// Words from the user dictionary.
    user: HashSet<String>,
    file: Option<PathBuf>,
    file_mtime: Option<SystemTime>,
    last_look: Instant,
    /// word → correct?
    cache: RefCell<HashMap<String, bool>>,
}

impl Speller {
    /// Loads the dictionary on a background thread; nothing is flagged until it is ready.
    pub fn new() -> Self {
        Self::with_file(user_dict_path())
    }

    pub fn with_file(file: Option<PathBuf>) -> Self {
        let mut s = Self {
            dict: None,
            rx: None,
            user: HashSet::new(),
            file,
            file_mtime: None,
            last_look: Instant::now(),
            cache: RefCell::new(HashMap::new()),
        };
        s.load();
        s
    }

    fn load(&mut self) {
        let (tx, rx) = channel();
        let file = self.file.clone();
        self.file_mtime = mtime(&file);
        std::thread::spawn(move || {
            let Ok(mut d) = Dictionary::new(AFF, DIC) else { return };
            for w in TECH_WORDS.split_whitespace() {
                let _ = d.add(w);
            }
            let text = file.and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
            let user: HashSet<String> = parse_user(&text).into_iter().collect();
            for w in &user {
                let _ = d.add(w);
            }
            let _ = tx.send((d, user));
        });
        self.rx = Some(rx);
    }

    /// Picks up a loaded dictionary, and reloads when the user dictionary file changed.
    /// Returns true when the words in effect changed (repaint).
    pub fn poll(&mut self) -> bool {
        if self.rx.is_none() && self.last_look.elapsed().as_secs_f32() > 1.5 {
            self.last_look = Instant::now();
            if mtime(&self.file) != self.file_mtime {
                self.load();
            }
        }
        let Some(rx) = &self.rx else { return false };
        match rx.try_recv() {
            Ok((d, user)) => {
                self.dict = Some(d);
                self.user = user;
                self.cache.borrow_mut().clear();
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

    /// Whether `word` is accepted because of the user dictionary.
    pub fn is_user_word(&self, word: &str) -> bool {
        self.user.contains(word)
    }

    /// The user dictionary's words, sorted.
    pub fn user_words(&self) -> Vec<String> {
        let mut v: Vec<String> = self.user.iter().cloned().collect();
        v.sort_by_key(|w| w.to_lowercase());
        v
    }

    /// The user dictionary file, created with a short explanation if it doesn't exist yet.
    pub fn ensure_file(&self) -> Result<PathBuf, String> {
        let path = self.file.clone().ok_or("no config folder")?;
        if !path.exists() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            std::fs::write(&path, HEADER).map_err(|e| e.to_string())?;
        }
        Ok(path)
    }

    /// Accept a word from now on and remember it in the user dictionary.
    pub fn add_word(&mut self, word: &str) -> Result<(), String> {
        let word = word.trim();
        if let Some(d) = &mut self.dict {
            d.add(word).map_err(|e| e.to_string())?;
        }
        self.user.insert(word.to_string());
        self.cache.borrow_mut().remove(word);
        let path = self.ensure_file()?;
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).map_err(|e| e.to_string())?;
        writeln!(f, "{word}").map_err(|e| e.to_string())?;
        // already in effect: no reload for our own write
        self.file_mtime = mtime(&self.file);
        Ok(())
    }

    /// Take a word out of the user dictionary (reloads the dictionary in the background).
    pub fn remove_word(&mut self, word: &str) -> Result<(), String> {
        let path: &Path = self.file.as_deref().ok_or("no config folder")?;
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        std::fs::write(path, remove_line(&text, word)).map_err(|e| e.to_string())?;
        self.user.remove(word);
        self.load();
        Ok(())
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

    fn wait(s: &mut Speller) {
        let t0 = std::time::Instant::now();
        while !s.poll() {
            assert!(t0.elapsed().as_secs() < 30, "dictionary did not load");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn dictionary_checks_and_suggests() {
        let mut s = Speller::with_file(None);
        wait(&mut s);
        assert_eq!(s.typos("This sentense has a speling mistake.", &[(usize::MAX, true)]), vec![(5, 13), (20, 27)]);
        assert!(s.check("Hello") && s.check("doesn't") && s.check("doesn’t") && s.check("config"));
        assert!(s.suggest("sentense").iter().any(|w| w == "sentence"));
    }

    #[test]
    fn user_dictionary_add_remove_and_reload() {
        let file = std::env::temp_dir().join(format!("edit-dict-test-{}.txt", std::process::id()));
        std::fs::write(&file, "# comment\nfrobnicate\n").unwrap();
        let mut s = Speller::with_file(Some(file.clone()));
        wait(&mut s);
        assert!(s.check("frobnicate") && s.is_user_word("frobnicate"));
        assert!(!s.check("Kubernetes"));
        s.add_word("Kubernetes").unwrap();
        assert!(s.check("Kubernetes"));
        assert_eq!(s.user_words(), ["frobnicate", "Kubernetes"]);
        s.remove_word("frobnicate").unwrap();
        wait(&mut s);
        assert!(!s.check("frobnicate") && s.check("Kubernetes"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "# comment\nKubernetes\n");
        // edited elsewhere (e.g. in a tab): picked up on the next look at the file
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&file, "grokking\n").unwrap();
        s.last_look = Instant::now() - std::time::Duration::from_secs(5);
        s.file_mtime = None; // coarse file times may not have ticked
        wait(&mut s);
        assert!(s.check("grokking") && !s.check("Kubernetes"));
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn user_file_parsing() {
        assert_eq!(parse_user("# hi\n  foo \n\nbar\n"), ["foo", "bar"]);
        assert_eq!(remove_line("# hi\nfoo\nbar\n foo \n", "foo"), "# hi\nbar\n");
    }
}
