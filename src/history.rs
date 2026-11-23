//! Local history, like IntelliJ's: versions of a file are recorded when it is opened, saved,
//! changed on disk by another program, or rewritten by Replace in Files.
//!
//! Recording costs the UI thread almost nothing: it hands over a rope clone (O(1), shared
//! structure) and a background thread does the hashing, compression and disk writes.
//! Storage, under `<config>/history/`:
//! - `blobs/<fnv64>-<len>.z` deflate-compressed text, content-addressed (stored once)
//! - `index/<fnv64 of path>.log` `path\t<path>` header, then one line per version:
//!   `<unix ms>\t<event>\t<blob>\t<chars>`
//!
//! Versions older than 30 days, or beyond 200 per file, are pruned once a day, together
//! with blobs no version uses any more.

use crate::highlight::config_dir;
use ropey::Rope;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const KEEP_DAYS: u64 = 30;
const KEEP_VERSIONS: usize = 200;
const MAX_CHARS: usize = 5 << 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    Opened,
    Saved,
    ChangedOnDisk,
    BeforeReplace,
    Replaced,
    Reverted,
}

impl Event {
    fn tag(self) -> &'static str {
        match self {
            Event::Opened => "opened",
            Event::Saved => "saved",
            Event::ChangedOnDisk => "external",
            Event::BeforeReplace => "before-replace",
            Event::Replaced => "replaced",
            Event::Reverted => "reverted",
        }
    }

    pub fn label(tag: &str) -> &'static str {
        match tag {
            "opened" => "Opened",
            "saved" => "Saved",
            "external" => "Changed on disk",
            "before-replace" => "Before Replace in Files",
            "replaced" => "Replace in Files",
            "reverted" => "Reverted from Local History",
            _ => "Version",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Version {
    /// Unix time in milliseconds.
    pub time: u64,
    pub event: String,
    blob: String,
    /// Length in chars.
    pub chars: usize,
}

enum Job {
    Record { path: PathBuf, text: Rope, event: Event, time: u64 },
}

pub struct History {
    dir: Option<PathBuf>,
    tx: Option<Sender<Job>>,
    worker: Option<JoinHandle<()>>,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// FNV-1a: stable across Rust versions and platforms (unlike `DefaultHasher`).
fn fnv(bytes: impl IntoIterator<Item = u8>) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn log_name(path: &Path) -> String {
    format!("{:016x}.log", fnv(path.to_string_lossy().bytes()))
}

impl History {
    pub fn start() -> Self {
        let Some(dir) = config_dir().map(|d| d.join("history")) else {
            return Self { dir: None, tx: None, worker: None };
        };
        let (tx, rx) = channel::<Job>();
        let root = dir.clone();
        let worker = std::thread::Builder::new()
            .name("local-history".into())
            .spawn(move || {
                let _ = std::fs::create_dir_all(root.join("blobs"));
                let _ = std::fs::create_dir_all(root.join("index"));
                prune_daily(&root);
                // last blob per file, so re-recording identical content is a no-op
                let mut last: HashMap<PathBuf, String> = HashMap::new();
                while let Ok(Job::Record { path, text, event, time }) = rx.recv() {
                    let _ = write_version(&root, &mut last, &path, &text, event, time);
                }
            })
            .ok();
        Self { dir: Some(dir), tx: Some(tx), worker }
    }

    /// Record `text` as a version of `path` (in the background).
    pub fn record(&self, path: &Path, text: &Rope, event: Event) {
        if text.len_chars() > MAX_CHARS {
            return;
        }
        if let Some(tx) = &self.tx {
            let _ = tx.send(Job::Record { path: path.to_path_buf(), text: text.clone(), event, time: now_ms() });
        }
    }

    pub fn record_str(&self, path: &Path, text: &str, event: Event) {
        // stored like the editor holds it: no BOM, LF line endings
        let t = text.strip_prefix('\u{feff}').unwrap_or(text);
        let t = if t.contains("\r\n") { t.replace("\r\n", "\n") } else { t.to_string() };
        self.record(path, &Rope::from_str(&t), event);
    }

    /// Versions of `path`, newest first.
    pub fn versions(&self, path: &Path) -> Vec<Version> {
        let Some(dir) = &self.dir else { return vec![] };
        let mut v = read_log(&dir.join("index").join(log_name(path))).map(|(_, v)| v).unwrap_or_default();
        v.reverse();
        v
    }

    /// Change sets of `path`, newest first.
    pub fn change_sets_of(&self, path: &Path) -> Vec<ChangeSet> {
        let mut v = self.versions(path);
        v.reverse();
        change_sets(vec![(path.to_path_buf(), v)])
    }

    /// Change sets touching files under `dir` (including files deleted since), newest first.
    pub fn change_sets_under(&self, dir: &Path) -> Vec<ChangeSet> {
        let Some(root) = &self.dir else { return vec![] };
        let mut logs = vec![];
        if let Ok(rd) = std::fs::read_dir(root.join("index")) {
            for e in rd.flatten() {
                if let Some((file, versions)) = read_log(&e.path()) {
                    if file.starts_with(dir) {
                        logs.push((file, versions));
                    }
                }
            }
        }
        change_sets(logs)
    }

    pub fn load(&self, v: &Version) -> Option<String> {
        let bytes = std::fs::read(self.dir.as_ref()?.join("blobs").join(&v.blob)).ok()?;
        let raw = miniz_oxide::inflate::decompress_to_vec(&bytes).ok()?;
        String::from_utf8(raw).ok()
    }

    /// Finish pending writes (on quit).
    pub fn shutdown(&mut self) {
        self.tx = None;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

fn read_log(path: &Path) -> Option<(PathBuf, Vec<Version>)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let file = PathBuf::from(lines.next()?.strip_prefix("path\t")?);
    let versions = lines
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (time, event, blob, chars) = (f.next()?.parse().ok()?, f.next()?, f.next()?, f.next()?.parse().ok()?);
            // blob names are generated by us; reject anything that could leave blobs/
            let ok = blob.len() < 64 && blob.chars().all(|c| c.is_ascii_hexdigit() || c == '-' || c == '.' || c == 'z');
            ok.then(|| Version { time, event: event.to_string(), blob: blob.to_string(), chars })
        })
        .collect();
    Some((file, versions))
}

fn write_version(
    root: &Path,
    last: &mut HashMap<PathBuf, String>,
    path: &Path,
    text: &Rope,
    event: Event,
    time: u64,
) -> std::io::Result<()> {
    let log = root.join("index").join(log_name(path));
    if !last.contains_key(path) {
        let prev = read_log(&log).and_then(|(_, v)| v.last().map(|v| v.blob.clone()));
        last.insert(path.to_path_buf(), prev.unwrap_or_default());
    }
    let hash = fnv(text.bytes());
    let blob = format!("{hash:016x}-{}.z", text.len_bytes());
    if last.get(path) == Some(&blob) {
        return Ok(()); // same as the newest version
    }
    let blob_path = root.join("blobs").join(&blob);
    if !blob_path.exists() {
        let data = miniz_oxide::deflate::compress_to_vec(text.to_string().as_bytes(), 6);
        let tmp = blob_path.with_extension("tmp");
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, &blob_path)?;
    }
    let new_log = !log.exists();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&log)?;
    if new_log {
        writeln!(f, "path\t{}", path.display())?;
    }
    writeln!(f, "{time}\t{}\t{blob}\t{}", event.tag(), text.len_chars())?;
    last.insert(path.to_path_buf(), blob);
    Ok(())
}

/// Drop old versions and unused blobs, at most once a day.
fn prune_daily(root: &Path) {
    let marker = root.join("last-prune");
    let fresh = std::fs::metadata(&marker)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().unwrap_or_default() < Duration::from_secs(24 * 3600));
    if fresh {
        return;
    }
    let _ = std::fs::write(&marker, b"");
    let cutoff = now_ms().saturating_sub(KEEP_DAYS * 24 * 3600 * 1000);
    let mut used = HashSet::new();
    if let Ok(rd) = std::fs::read_dir(root.join("index")) {
        for e in rd.flatten() {
            let p = e.path();
            let Some((file, mut v)) = read_log(&p) else { continue };
            let before = v.len();
            v.retain(|x| x.time >= cutoff);
            if v.len() > KEEP_VERSIONS {
                v.drain(..v.len() - KEEP_VERSIONS);
            }
            if v.is_empty() {
                let _ = std::fs::remove_file(&p);
                continue;
            }
            used.extend(v.iter().map(|x| x.blob.clone()));
            if v.len() != before {
                let mut s = format!("path\t{}\n", file.display());
                for x in &v {
                    s += &format!("{}\t{}\t{}\t{}\n", x.time, x.event, x.blob, x.chars);
                }
                let tmp = p.with_extension("tmp");
                if std::fs::write(&tmp, s).is_ok() {
                    let _ = std::fs::rename(&tmp, &p);
                }
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(root.join("blobs")) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            // leave recent files alone: another running instance may be writing them
            let recent = e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().unwrap_or_default() < Duration::from_secs(3600));
            if !used.contains(&name) && !recent {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

// ------------------------------------------------------------------ change sets

/// Changes this close together (and not further apart from the first) form one change set.
const SET_GAP_MS: u64 = 3 * 60 * 1000;
const SET_SPAN_MS: u64 = 15 * 60 * 1000;
/// The files of one Replace in Files are recorded within moments of each other.
const REPLACE_GAP_MS: u64 = 2000;

/// How one file changed within a change set.
#[derive(Clone, Debug, PartialEq)]
pub struct FileChange {
    pub path: PathBuf,
    /// The file before the set, and after it.
    pub before: Version,
    pub after: Version,
    /// Versions recorded for this file in the set.
    pub count: usize,
}

/// Changes made together: one Replace in Files, or edits within a few minutes.
#[derive(Clone, Debug, PartialEq)]
pub struct ChangeSet {
    pub start: u64,
    pub end: u64,
    pub replace: bool,
    /// The distinct events recorded in the set.
    pub events: Vec<String>,
    /// Sorted by path.
    pub files: Vec<FileChange>,
}

impl ChangeSet {
    /// "Replace in Files", "Saved", "Changed on disk"… or "Changes" when mixed.
    pub fn label(&self) -> &'static str {
        if self.replace {
            return "Replace in Files";
        }
        match self.events.as_slice() {
            [one] => Event::label(one),
            _ => "Changes",
        }
    }
}

/// Group the version logs of files into change sets, newest first. A file's first version
/// is its baseline (usually from opening it), not a change, so it starts no set.
pub fn change_sets(logs: Vec<(PathBuf, Vec<Version>)>) -> Vec<ChangeSet> {
    // every version with a predecessor is a change: (time, path, before, after)
    let mut changes: Vec<(u64, PathBuf, Version, Version)> = vec![];
    for (path, mut versions) in logs {
        versions.sort_by_key(|v| v.time);
        for w in versions.windows(2) {
            changes.push((w[1].time, path.clone(), w[0].clone(), w[1].clone()));
        }
    }
    changes.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut sets: Vec<ChangeSet> = vec![];
    for (time, path, before, after) in changes {
        let replace = after.event == Event::Replaced.tag();
        let joins = sets.last().is_some_and(|s| {
            s.replace == replace
                && time.saturating_sub(s.end) <= if replace { REPLACE_GAP_MS } else { SET_GAP_MS }
                && (replace || time.saturating_sub(s.start) <= SET_SPAN_MS)
        });
        if !joins {
            sets.push(ChangeSet { start: time, end: time, replace, events: vec![], files: vec![] });
        }
        let s = sets.last_mut().unwrap();
        s.end = time;
        if !s.events.contains(&after.event) {
            s.events.push(after.event.clone());
        }
        match s.files.iter_mut().find(|f| f.path == path) {
            Some(f) => {
                f.after = after;
                f.count += 1;
            }
            None => s.files.push(FileChange { path, before, after, count: 1 }),
        }
    }
    for s in &mut sets {
        s.files.sort_by(|a, b| a.path.cmp(&b.path));
    }
    sets.reverse();
    sets
}


// ------------------------------------------------------------------ time display

static LOCAL_OFFSET: OnceLock<time::UtcOffset> = OnceLock::new();

/// Read the local UTC offset; call early in `main`, before other threads start (on Unix
/// the offset can only be read safely while the process is single-threaded).
pub fn init_local_offset() {
    let off = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    let _ = LOCAL_OFFSET.set(off);
}

/// ("14:32:05", "today" / "yesterday" / "2026-11-20") in local time.
pub fn format_time(ms: u64) -> (String, String) {
    let off = *LOCAL_OFFSET.get().unwrap_or(&time::UtcOffset::UTC);
    let at = |ms: u64| {
        time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
            .to_offset(off)
    };
    let t = at(ms);
    let today = at(now_ms()).date();
    let day = if t.date() == today {
        "today".to_string()
    } else if today.previous_day() == Some(t.date()) {
        "yesterday".to_string()
    } else {
        format!("{}-{:02}-{:02}", t.year(), t.month() as u8, t.day())
    };
    (format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second()), day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_dedupes_and_loads() {
        let root = std::env::temp_dir().join(format!("edit-history-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("blobs")).unwrap();
        std::fs::create_dir_all(root.join("index")).unwrap();
        let mut last = HashMap::new();
        let file = PathBuf::from("/proj/a.rs");
        let v1 = Rope::from_str("fn main() {}\n");
        let v2 = Rope::from_str("fn main() { run(); }\n");
        write_version(&root, &mut last, &file, &v1, Event::Opened, 1).unwrap();
        write_version(&root, &mut last, &file, &v1, Event::Saved, 2).unwrap(); // same: skipped
        write_version(&root, &mut last, &file, &v2, Event::Saved, 3).unwrap();
        // a fresh writer (new run) also knows the newest version
        write_version(&root, &mut HashMap::new(), &file, &v2, Event::Opened, 4).unwrap();
        let h = History { dir: Some(root.clone()), tx: None, worker: None };
        let v = h.versions(&file);
        assert_eq!(v.iter().map(|x| (x.time, x.event.as_str())).collect::<Vec<_>>(), vec![(3, "saved"), (1, "opened")]);
        assert_eq!(h.load(&v[0]).unwrap(), "fn main() { run(); }\n");
        assert_eq!(h.load(&v[1]).unwrap(), "fn main() {}\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn folder_change_sets_skip_baselines_and_group_by_time() {
        let root = std::env::temp_dir().join(format!("edit-history-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("blobs")).unwrap();
        std::fs::create_dir_all(root.join("index")).unwrap();
        let mut last = HashMap::new();
        let proj = PathBuf::from("/proj");
        let text = |s: &str| Rope::from_str(s);
        let min = 60_000;
        let mut w = |p: &Path, s: &str, e: Event, t: u64| write_version(&root, &mut last, p, &text(s), e, t).unwrap();
        let (a, b, c) = (proj.join("a.rs"), proj.join("sub").join("b.rs"), proj.join("c.rs"));
        w(&a, "a0", Event::Opened, 0);
        w(&b, "b0", Event::Opened, 0);
        w(&c, "c0", Event::Opened, 0); // only opened: never listed
        w(&a, "a1", Event::Saved, 10 * min);
        w(&a, "a2", Event::Saved, 11 * min);
        w(&b, "b1", Event::Saved, 12 * min); // same set: within 3 minutes
        w(&a, "a3", Event::Saved, 30 * min); // new set
        w(&a, "a3x", Event::BeforeReplace, 30 * min + 500); // joins: edits right before the replace
        w(&a, "a4", Event::Replaced, 30 * min + 600); // a replace is its own set
        w(&b, "b2", Event::Replaced, 30 * min + 700);
        w(Path::new("/project-other/x.rs"), "x0", Event::Opened, 0);
        w(Path::new("/project-other/x.rs"), "x1", Event::Saved, 12 * min);
        let h = History { dir: Some(root.clone()), tx: None, worker: None };
        let sets = h.change_sets_under(&proj);
        let show = |s: &ChangeSet| {
            let files: Vec<String> = s.files.iter().map(|f| {
                let rel = f.path.strip_prefix(&proj).unwrap().to_string_lossy().replace('\\', "/");
                format!("{rel}:{}>{}", h.load(&f.before).unwrap(), h.load(&f.after).unwrap())
            }).collect();
            (s.label(), files.join(" "))
        };
        assert_eq!(
            sets.iter().map(show).collect::<Vec<_>>(),
            vec![
                ("Replace in Files", "a.rs:a3x>a4 sub/b.rs:b1>b2".to_string()),
                ("Changes", "a.rs:a2>a3x".to_string()),
                ("Saved", "a.rs:a0>a2 sub/b.rs:b0>b1".to_string()),
            ]
        );
        assert_eq!(h.change_sets_of(&c), vec![]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn long_editing_sessions_are_split() {
        let v = |t: u64| Version { time: t * 60_000, event: "saved".into(), blob: format!("{t}"), chars: 1 };
        // a save every 2 minutes for 40 minutes: no gap, but sets span at most 15 minutes
        let sets = change_sets(vec![(PathBuf::from("/a"), (0..=20).map(|i| v(i * 2)).collect())]);
        assert_eq!(sets.len(), 3);
        assert!(sets.iter().all(|s| s.end - s.start <= SET_SPAN_MS));
        assert_eq!(sets[0].label(), "Saved");
        assert_eq!(sets.iter().map(|s| s.files[0].count).sum::<usize>(), 20);
    }

    #[test]
    fn rejects_unsafe_blob_names() {
        let root = std::env::temp_dir().join(format!("edit-history-bad-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let log = root.join("x.log");
        std::fs::write(&log, "path\t/a\n1\tsaved\t..\\..\\secret\t3\n2\tsaved\tabc-1.z\t3\n").unwrap();
        let (_, v) = read_log(&log).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].blob, "abc-1.z");
        let _ = std::fs::remove_dir_all(&root);
    }
}
