//! Git status for the sidebar (IntelliJ-style file colours) and the branch in the status bar.
//!
//! `git status` runs on a background thread, one refresh at a time; the app asks for a
//! refresh when files or the repository change (see `App::sync_fs`).

use eframe::egui::{self, Color32};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{channel, Receiver};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Status {
    // ordered by priority: a folder shows its most important child
    Ignored,
    Untracked,
    Modified,
    Added,
    Conflict,
}

impl Status {
    /// IntelliJ (Darcula) file status colours.
    pub fn color(self) -> Color32 {
        match self {
            Status::Ignored => Color32::from_rgb(0x84, 0x85, 0x04),
            Status::Untracked => Color32::from_rgb(0xd1, 0x67, 0x5a),
            Status::Modified => Color32::from_rgb(0x68, 0x97, 0xbb),
            Status::Added => Color32::from_rgb(0x62, 0x97, 0x55),
            Status::Conflict => Color32::from_rgb(0xff, 0x55, 0x55),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Status::Ignored => "ignored",
            Status::Untracked => "untracked",
            Status::Modified => "modified",
            Status::Added => "added",
            Status::Conflict => "merge conflict",
        }
    }
}

/// Status of one sidebar folder that is (inside) a Git repository.
#[derive(Default, Debug, PartialEq)]
pub struct RootStatus {
    pub branch: String,
    /// Files (and whole untracked/ignored folders) with a status, by absolute path.
    pub files: HashMap<PathBuf, Status>,
    /// Folders containing changed files, with their most important status.
    pub dirs: HashMap<PathBuf, Status>,
    /// The latest move of HEAD, if it was a commit (for Local History labels).
    pub commit: Option<Commit>,
}

impl RootStatus {
    /// Status of `path`, including what it inherits from an untracked or ignored folder.
    pub fn of(&self, path: &Path) -> Option<Status> {
        if let Some(s) = self.files.get(path) {
            return Some(*s);
        }
        path.ancestors()
            .skip(1)
            .find_map(|a| self.files.get(a).filter(|s| matches!(s, Status::Untracked | Status::Ignored)).copied())
    }
}

fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flash
    }
    cmd
}

fn run(cmd: &mut Command) -> Option<String> {
    let out = cmd.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Parse `git status --porcelain=v1 -z --branch --ignored` output. Paths in it are
/// relative to the repository root; `prefix` is the sidebar folder's path inside the
/// repository (`git rev-parse --show-prefix`, e.g. `sub/dir/`), and only entries under it
/// are kept, joined onto `root` so they match the sidebar's own paths exactly.
pub fn parse(out: &str, root: &Path, prefix: &str) -> RootStatus {
    let mut st = RootStatus::default();
    let mut entries = out.split('\0');
    while let Some(e) = entries.next() {
        if let Some(b) = e.strip_prefix("## ") {
            // "## main...origin/main [ahead 1]", "## HEAD (no branch)", "## No commits yet on main"
            let b = b.strip_prefix("No commits yet on ").unwrap_or(b);
            st.branch = b.split("...").next().unwrap_or(b).split(' ').next().unwrap_or("").to_string();
            continue;
        }
        if e.len() < 4 {
            continue;
        }
        let (xy, path) = e.split_at(3);
        let (x, y) = (xy.as_bytes()[0], xy.as_bytes()[1]);
        if x == b'R' || x == b'C' {
            entries.next(); // the original name of a rename/copy follows
        }
        let status = match (x, y) {
            (b'?', b'?') => Status::Untracked,
            (b'!', b'!') => Status::Ignored,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => Status::Conflict,
            (b'D', _) | (_, b'D') => continue, // deleted: not in the tree
            (b'A', _) => Status::Added,
            _ => Status::Modified,
        };
        let Some(rel) = path.strip_prefix(prefix) else { continue };
        let rel = rel.trim_end_matches('/');
        if rel.is_empty() {
            continue;
        }
        let mut abs = root.to_path_buf();
        abs.extend(rel.split('/'));
        if status != Status::Ignored {
            for dir in abs.ancestors().skip(1) {
                if dir == root || !dir.starts_with(root) {
                    break;
                }
                let s = st.dirs.entry(dir.to_path_buf()).or_insert(status);
                *s = (*s).max(status);
            }
        }
        st.files.insert(abs, status);
    }
    st
}

/// A commit HEAD moved to.
#[derive(Clone, Debug, PartialEq)]
pub struct Commit {
    pub hash: String,
    /// Commit time, Unix ms.
    pub time: u64,
    /// "Commit: message", "Commit (amend): message", …
    pub label: String,
}

/// Parse `git log -g -1 --format=%H%x09%ct%x09%gs`: the newest reflog entry of HEAD, kept
/// only when it records a commit (not a checkout, reset, pull…).
pub fn parse_reflog(out: &str) -> Option<Commit> {
    let mut f = out.trim_end_matches(['\r', '\n']).splitn(3, '\t');
    let (hash, time, subject) = (f.next()?, f.next()?.parse::<u64>().ok()?, f.next()?);
    let rest = subject.strip_prefix("commit")?;
    if !(rest.starts_with(':') || rest.starts_with(" (")) || hash.len() < 7 {
        return None;
    }
    Some(Commit { hash: hash.to_string(), time: time * 1000, label: format!("Commit{rest}") })
}

/// Status of one sidebar folder, or None if it isn't in a Git repository (or git is missing).
pub fn status(root: &Path) -> Option<RootStatus> {
    let prefix = run(git(root).args(["rev-parse", "--show-prefix"]))?;
    let out = run(git(root).args(["status", "--porcelain=v1", "-z", "--branch", "--ignored"]))?;
    let mut st = parse(&out, root, prefix.trim());
    st.commit = run(git(root).args(["log", "-g", "-1", "--format=%H%x09%ct%x09%gs"])).and_then(|s| parse_reflog(&s));
    Some(st)
}

/// Background refreshes, one at a time.
pub struct GitWatch {
    rx: Option<Receiver<Vec<(PathBuf, RootStatus)>>>,
    /// Status per sidebar folder (absent: not a repository).
    pub roots: HashMap<PathBuf, RootStatus>,
}

impl GitWatch {
    pub fn new() -> Self {
        Self { rx: None, roots: HashMap::new() }
    }

    pub fn busy(&self) -> bool {
        self.rx.is_some()
    }

    pub fn refresh(&mut self, roots: Vec<PathBuf>, ctx: egui::Context) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let res = roots.into_iter().filter_map(|r| status(&r).map(|s| (r, s))).collect();
            let _ = tx.send(res);
            ctx.request_repaint();
        });
        self.rx = Some(rx);
    }

    /// Take a finished refresh; true if the status changed.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        match rx.try_recv() {
            Ok(res) => {
                self.rx = None;
                let new: HashMap<PathBuf, RootStatus> = res.into_iter().collect();
                let changed = new != self.roots;
                self.roots = new;
                changed
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.rx = None;
                false
            }
            Err(_) => false,
        }
    }

    fn root_of(&self, path: &Path) -> Option<&RootStatus> {
        // the innermost sidebar folder wins when folders are nested
        self.roots.iter().filter(|(r, _)| path.starts_with(r)).max_by_key(|(r, _)| r.as_os_str().len()).map(|(_, s)| s)
    }

    /// Status of a file in any sidebar folder.
    pub fn file(&self, path: &Path) -> Option<Status> {
        self.root_of(path)?.of(path)
    }

    /// Status of a folder: its most important changed file, or its own untracked/ignored state.
    pub fn dir(&self, path: &Path) -> Option<Status> {
        let s = self.root_of(path)?;
        s.dirs.get(path).copied().or_else(|| s.of(path))
    }

    /// Branch of the repository containing `path` (or of the first repository).
    pub fn branch(&self, path: Option<&Path>) -> Option<&str> {
        let hit = path.and_then(|p| self.roots.iter().find(|(r, _)| p.starts_with(r)));
        hit.or_else(|| self.roots.iter().next()).map(|(_, s)| s.branch.as_str()).filter(|b| !b.is_empty())
    }
}

/// Does a change at `path` (from the file watcher) possibly change `git status`?
pub fn affects_status(path: &Path, skip_dirs: &[&str]) -> bool {
    let names: Vec<&std::ffi::OsStr> = path.components().map(|c| c.as_os_str()).collect();
    if let Some(i) = names.iter().position(|n| *n == ".git") {
        // inside .git: the index (staging, commits), HEAD (checkout) and refs
        return names[i + 1..].first().is_some_and(|n| *n == "index" || *n == "HEAD" || *n == "refs");
    }
    !names.iter().any(|n| skip_dirs.iter().any(|s| n == s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_relative_to_the_sidebar_folder() {
        let root = PathBuf::from("/repo/app");
        let out = "## main...origin/main [ahead 1]\0 M app/src/main.rs\0A  app/new.rs\0R  app/b.rs\0app/old_b.rs\0\
                   ?? app/scratch/\0!! app/target/\0UU app/src/merge.rs\0 D app/gone.rs\0 M other/x.rs\0";
        let st = parse(out, &root, "app/");
        assert_eq!(st.branch, "main");
        let p = |s: &str| root.join(s);
        assert_eq!(st.files.get(&p("src/main.rs")), Some(&Status::Modified));
        assert_eq!(st.files.get(&p("new.rs")), Some(&Status::Added));
        assert_eq!(st.files.get(&p("b.rs")), Some(&Status::Modified));
        assert!(!st.files.contains_key(&p("old_b.rs")) && !st.files.contains_key(&p("gone.rs")));
        assert_eq!(st.files.len(), 6); // main, new, b, scratch, target, merge
        // folders take their most important child; ignored doesn't colour folders
        assert_eq!(st.dirs.get(&p("src")), Some(&Status::Conflict));
        // children of untracked / ignored folders inherit their status
        assert_eq!(st.of(&p("scratch/notes.txt")), Some(Status::Untracked));
        assert_eq!(st.of(&p("target/debug/edit.exe")), Some(Status::Ignored));
        assert_eq!(st.of(&p("src/lib.rs")), None);
    }

    #[test]
    fn branch_variants() {
        assert_eq!(parse("## No commits yet on master\0", Path::new("/r"), "").branch, "master");
        assert_eq!(parse("## feature/x\0", Path::new("/r"), "").branch, "feature/x");
    }

    #[test]
    fn commits_from_the_reflog() {
        let c = parse_reflog("0123456789abcdef\t1700000000\tcommit: Fix the thing\n").unwrap();
        assert_eq!((c.hash.as_str(), c.time, c.label.as_str()), ("0123456789abcdef", 1_700_000_000_000, "Commit: Fix the thing"));
        assert_eq!(parse_reflog("0123456789abcdef\t1\tcommit (amend): x").unwrap().label, "Commit (amend): x");
        assert_eq!(parse_reflog("0123456789abcdef\t1\tcommit (initial): x").unwrap().label, "Commit (initial): x");
        assert_eq!(parse_reflog("0123456789abcdef\t1\tcheckout: moving from a to b"), None);
        assert_eq!(parse_reflog("0123456789abcdef\t1\tcommitted: x"), None);
        assert_eq!(parse_reflog(""), None);
    }

    #[test]
    fn which_changes_trigger_a_refresh() {
        let skip = &[".git", "target", "node_modules"];
        assert!(affects_status(Path::new("/r/src/a.rs"), skip));
        assert!(affects_status(Path::new("/r/.git/index"), skip));
        assert!(affects_status(Path::new("/r/.git/refs/heads/main"), skip));
        assert!(!affects_status(Path::new("/r/.git/objects/ab/cdef"), skip));
        assert!(!affects_status(Path::new("/r/target/debug/x.o"), skip));
    }
}
