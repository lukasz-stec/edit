//! Session persistence: open files, carets, scroll, folder and view settings.
//! Stored as a simple tab-separated text file in %APPDATA%\edit\session.txt.

use crate::highlight::config_dir;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct FileState {
    pub path: PathBuf,
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

fn flag(b: bool) -> &'static str {
    if b {
        "1"
    } else {
        "0"
    }
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
            s += &format!("file\t{}\t{}\t{}\t{}\n", f.anchor, f.head, f.scroll_y, f.path.display());
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
                    // path goes last so it may contain anything but a newline
                    let f: Vec<&str> = val.splitn(4, '\t').collect();
                    if let [a, h, y, p] = f[..] {
                        s.files.push(FileState {
                            path: PathBuf::from(p),
                            anchor: a.parse().unwrap_or(0),
                            head: h.parse().unwrap_or(0),
                            scroll_y: y.parse().unwrap_or(0.0),
                        });
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
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)
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
                FileState { path: PathBuf::from(r"C:\proj\my app\src\main.rs"), anchor: 10, head: 42, scroll_y: 123.5 },
                FileState { path: PathBuf::from(r"C:\notes.txt"), anchor: 0, head: 0, scroll_y: 0.0 },
            ],
            active: 1,
            theme: Some("base16-ocean.dark".into()),
            font_size: Some(15.0),
            sidebar: Some(true),
            minimap: Some(false),
            camel_humps: Some(true),
        };
        assert_eq!(Session::parse(&s.to_text()), s);
    }

    #[test]
    fn ignores_garbage() {
        let s = Session::parse("nonsense\nfile\tx\nactive\tq\n");
        assert!(s.files.is_empty());
        assert_eq!(s.active, 0);
    }
}
