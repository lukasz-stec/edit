//! Session persistence ("hot exit"): open files, carets, scroll, folder and view settings,
//! plus the contents of unsaved buffers.
//!
//! - `%APPDATA%\edit\session.txt`   simple tab-separated text, one tab per line
//! - `%APPDATA%\edit\backups\*.txt` contents of unsaved buffers referenced by the session

use crate::highlight::config_dir;
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct FileState {
    /// None for an untitled buffer.
    pub path: Option<PathBuf>,
    /// Backup file name holding unsaved contents (only for dirty buffers).
    pub backup: Option<String>,
    /// Line endings to use when the restored buffer is eventually saved.
    pub crlf: bool,
    pub anchor: usize,
    pub head: usize,
    pub scroll_y: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Session {
    pub root: Option<PathBuf>,
    pub files: Vec<FileState>,
    pub active: usize,
    pub theme: Option<String>,
    pub font_size: Option<f32>,
    pub sidebar: Option<bool>,
    pub minimap: Option<bool>,
    pub camel_humps: Option<bool>,
}

fn file_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("session.txt"))
}

fn backup_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("backups"))
}

fn flag(b: bool) -> &'static str {
    if b {
        "1"
    } else {
        "0"
    }
}

/// Backup names come from the session file; only accept plain file names.
fn valid_backup_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\', ':']) && name != "." && name != ".."
}

fn write_atomic(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

impl Session {
    pub fn to_text(&self) -> String {
        let mut s = String::from("edit-session\t1\n");
        if let Some(r) = &self.root {
            s += &format!("root\t{}\n", r.display());
        }
        if let Some(t) = &self.theme {
            s += &format!("theme\t{t}\n");
        }
        if let Some(f) = self.font_size {
            s += &format!("font\t{f}\n");
        }
        if let Some(b) = self.sidebar {
            s += &format!("sidebar\t{}\n", flag(b));
        }
        if let Some(b) = self.minimap {
            s += &format!("minimap\t{}\n", flag(b));
        }
        if let Some(b) = self.camel_humps {
            s += &format!("camel\t{}\n", flag(b));
        }
        s += &format!("active\t{}\n", self.active);
        for f in &self.files {
            let path = f.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
            match &f.backup {
                // path goes last so it may contain anything but a newline
                None => s += &format!("file\t{}\t{}\t{}\t{path}\n", f.anchor, f.head, f.scroll_y),
                Some(b) => {
                    s += &format!("buffer\t{}\t{}\t{}\t{}\t{b}\t{path}\n", f.anchor, f.head, f.scroll_y, flag(f.crlf))
                }
            }
        }
        s
    }

    pub fn parse(text: &str) -> Self {
        let mut s = Session::default();
        for line in text.lines() {
            let mut parts = line.splitn(2, '\t');
            let (Some(key), Some(val)) = (parts.next(), parts.next()) else { continue };
            match key {
                "root" => s.root = Some(PathBuf::from(val)),
                "theme" => s.theme = Some(val.to_string()),
                "font" => s.font_size = val.parse().ok(),
                "sidebar" => s.sidebar = Some(val == "1"),
                "minimap" => s.minimap = Some(val == "1"),
                "camel" => s.camel_humps = Some(val == "1"),
                "active" => s.active = val.parse().unwrap_or(0),
                "file" => {
                    let f: Vec<&str> = val.splitn(4, '\t').collect();
                    if let [a, h, y, p] = f[..] {
                        if !p.is_empty() {
                            s.files.push(FileState {
                                path: Some(PathBuf::from(p)),
                                backup: None,
                                crlf: false,
                                anchor: a.parse().unwrap_or(0),
                                head: h.parse().unwrap_or(0),
                                scroll_y: y.parse().unwrap_or(0.0),
                            });
                        }
                    }
                }
                "buffer" => {
                    let f: Vec<&str> = val.splitn(6, '\t').collect();
                    if let [a, h, y, crlf, b, p] = f[..] {
                        if valid_backup_name(b) {
                            s.files.push(FileState {
                                path: (!p.is_empty()).then(|| PathBuf::from(p)),
                                backup: Some(b.to_string()),
                                crlf: crlf == "1",
                                anchor: a.parse().unwrap_or(0),
                                head: h.parse().unwrap_or(0),
                                scroll_y: y.parse().unwrap_or(0.0),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        s
    }

    pub fn load() -> Option<Self> {
        let text = std::fs::read_to_string(file_path()?).ok()?;
        Some(Self::parse(&text))
    }

    /// Write atomically (temp file + rename) so a crash never leaves a half-written session.
    pub fn save_text(text: &str) -> std::io::Result<()> {
        let path = file_path().ok_or_else(|| std::io::Error::other("no config dir"))?;
        write_atomic(&path, text)
    }

    pub fn backup_names(&self) -> HashSet<String> {
        self.files.iter().filter_map(|f| f.backup.clone()).collect()
    }
}

pub fn write_backup(name: &str, text: &str) -> std::io::Result<()> {
    let dir = backup_dir().ok_or_else(|| std::io::Error::other("no config dir"))?;
    write_atomic(&dir.join(name), text)
}

pub fn read_backup(name: &str) -> Option<String> {
    if !valid_backup_name(name) {
        return None;
    }
    std::fs::read_to_string(backup_dir()?.join(name)).ok()
}

/// Delete backup files no longer referenced by the saved session.
pub fn cleanup_backups(keep: &HashSet<String>) {
    let Some(dir) = backup_dir() else { return };
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !keep.contains(&name) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let s = Session {
            root: Some(PathBuf::from(r"C:\proj\my app")),
            files: vec![
                FileState {
                    path: Some(PathBuf::from(r"C:\proj\my app\src\main.rs")),
                    backup: None,
                    crlf: false,
                    anchor: 10,
                    head: 42,
                    scroll_y: 123.5,
                },
                FileState {
                    path: Some(PathBuf::from(r"C:\notes.txt")),
                    backup: Some("1700000000-3.txt".into()),
                    crlf: true,
                    anchor: 0,
                    head: 0,
                    scroll_y: 0.0,
                },
                FileState { path: None, backup: Some("1700000000-4.txt".into()), crlf: false, anchor: 5, head: 5, scroll_y: 0.0 },
            ],
            active: 2,
            theme: Some("base16-ocean.dark".into()),
            font_size: Some(15.0),
            sidebar: Some(true),
            minimap: Some(false),
            camel_humps: Some(true),
        };
        assert_eq!(Session::parse(&s.to_text()), s);
    }

    #[test]
    fn ignores_garbage_and_unsafe_backup_names() {
        let s = Session::parse("nonsense\nfile\tx\nactive\tq\nbuffer\t0\t0\t0\t0\t..\\..\\evil\t\n");
        assert!(s.files.is_empty());
        assert_eq!(s.active, 0);
    }
}
