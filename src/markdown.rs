//! Markdown editing helpers (used when Markdown editing is on and the file is Markdown):
//! list / quote continuation, list markers, emphasis toggling.

/// The list or quote marker at the start of a line.
#[derive(Debug, PartialEq)]
pub struct Marker {
    /// Leading whitespace.
    pub indent: String,
    /// Length in chars of indent + marker (where the item's text starts).
    pub len: usize,
    /// Marker for the next item: same bullet, next number, unchecked task box.
    pub next: String,
}

/// Parse `- item`, `* item`, `+ item`, `1. item`, `1) item`, `- [ ] task`, `> quote`
/// (and quoted lists like `> - item`).
pub fn marker(line: &str) -> Option<Marker> {
    let line = line.trim_end_matches(['\n', '\r']);
    let chars: Vec<char> = line.chars().collect();
    let mut i = chars.iter().take_while(|c| **c == ' ' || **c == '\t').count();
    let indent: String = chars[..i].iter().collect();
    let mut next = String::new();
    // quotes: "> ", ">> ", "> > "
    let q0 = i;
    while i < chars.len() && chars[i] == '>' {
        i += 1;
        while i < chars.len() && chars[i] == ' ' {
            i += 1;
        }
    }
    if i > q0 {
        let q: String = chars[q0..i].iter().collect();
        next += &q;
        if !q.ends_with(' ') {
            next.push(' ');
        }
    }
    let rest = &chars[i..];
    let space_after = |k: usize| rest.get(k) == Some(&' ') || rest.get(k) == Some(&'\t');
    let list = match rest.first() {
        Some(&c) if "-*+".contains(c) && space_after(1) => {
            // not a horizontal rule like "- - -" or "* * *"
            let line_rest: String = rest.iter().filter(|c| !c.is_whitespace()).collect();
            if line_rest.len() >= 3 && line_rest.chars().all(|x| x == c) {
                return None;
            }
            Some((2, format!("{c} ")))
        }
        Some(c) if c.is_ascii_digit() => {
            let n = rest.iter().take_while(|c| c.is_ascii_digit()).count();
            let num: String = rest[..n].iter().collect();
            match (rest.get(n), n <= 9) {
                (Some(&d @ ('.' | ')')), true) if space_after(n + 1) => {
                    let v: u64 = num.parse().ok()?;
                    Some((n + 2, format!("{}{d} ", v + 1)))
                }
                _ => None,
            }
        }
        _ => None,
    };
    let mut len = i;
    if let Some((k, m)) = list {
        len += k;
        next += &m;
        // task box: "[ ] ", "[x] "
        let after: String = chars[len..].iter().take(4).collect();
        if matches!(after.as_str(), "[ ] " | "[x] " | "[X] ") {
            len += 4;
            next += "[ ] ";
        } else if matches!(after.as_str(), "[ ]" | "[x]" | "[X]") && len + 3 == chars.len() {
            len += 3;
            next += "[ ] ";
        }
    } else if i == q0 {
        return None;
    }
    Some(Marker { indent: indent.clone(), len, next: format!("{indent}{next}") })
}

/// A heading, for the outline.
#[derive(Clone, Debug, PartialEq)]
pub struct Heading {
    /// 0-based line.
    pub line: usize,
    /// 1–6.
    pub level: usize,
    pub title: String,
}

/// Headings: `# ATX` ones and `Setext` ones (a line underlined with `===` / `---`), not
/// inside fenced code blocks.
pub fn headings<S: AsRef<str>>(lines: impl IntoIterator<Item = S>) -> Vec<Heading> {
    let mut out = vec![];
    let mut fence: Option<(char, usize)> = None;
    let mut prev: Option<(usize, String)> = None; // previous line, if it could be a Setext title
    for (i, line) in lines.into_iter().enumerate() {
        let line = line.as_ref().trim_end_matches(['\n', '\r']);
        let indent = line.chars().take_while(|c| *c == ' ').count();
        let t = line.trim();
        // fenced code: ``` or ~~~ (3+), closed by the same char, at least as long
        let fc = t.chars().next().filter(|c| *c == '`' || *c == '~');
        if let Some(c) = fc.filter(|_| indent < 4) {
            let n = t.chars().take_while(|x| *x == c).count();
            if n >= 3 {
                match fence {
                    None => fence = Some((c, n)),
                    Some((fc, fnum)) if fc == c && n >= fnum && t.chars().all(|x| x == c) => fence = None,
                    _ => {}
                }
                prev = None;
                continue;
            }
        }
        if fence.is_some() {
            continue;
        }
        if indent < 4 && t.starts_with('#') {
            let level = t.chars().take_while(|c| *c == '#').count();
            let rest = &t[level..];
            if level <= 6 && (rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t')) {
                // drop a closing "###"
                let title = rest.trim();
                let title = match title.trim_end_matches('#') {
                    s if s.is_empty() || s.ends_with(' ') => s.trim_end(),
                    _ => title,
                };
                out.push(Heading { line: i, level, title: title.to_string() });
                prev = None;
                continue;
            }
        }
        if let Some((pl, title)) = prev.take() {
            let setext = if !t.is_empty() && indent < 4 && t.chars().all(|c| c == '=') {
                Some(1)
            } else if t.len() >= 2 && indent < 4 && t.chars().all(|c| c == '-') {
                Some(2)
            } else {
                None
            };
            if let Some(level) = setext {
                out.push(Heading { line: pl, level, title });
                continue;
            }
        }
        let plain = !t.is_empty() && indent < 4 && marker(line).is_none() && !t.starts_with('|');
        prev = plain.then(|| (i, t.to_string()));
    }
    out
}

/// Toggle `mark` (e.g. `**`) around `text`: unwrap if it is already wrapped.
pub fn toggle_wrap(text: &str, mark: &str) -> String {
    let n = mark.len();
    let bold_inside = mark == "*" && text.starts_with("**") && !text.starts_with("***");
    if text.len() >= 2 * n && text.starts_with(mark) && text.ends_with(mark) && !bold_inside {
        text[n..text.len() - n].to_string()
    } else {
        format!("{mark}{text}{mark}")
    }
}

/// A single pasted URL (`https://…`, `www.…`, `mailto:…`), trimmed.
pub fn as_url(text: &str) -> Option<&str> {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();
    let scheme = ["http://", "https://", "ftp://", "file://", "mailto:", "www."].iter().any(|s| lower.starts_with(s));
    (scheme && t.len() > 7 && !t.chars().any(char::is_whitespace)).then_some(t)
}

/// Whether a path or URL names an image (query and fragment ignored).
pub fn is_image(path: &str) -> bool {
    let p = path.split(['?', '#']).next().unwrap_or(path).to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg", ".bmp"].iter().any(|e| p.ends_with(e))
}

/// A link destination: `<…>` when it contains spaces or parentheses.
pub fn link_dest(dest: &str) -> String {
    if dest.contains([' ', '(', ')']) {
        format!("<{dest}>")
    } else {
        dest.to_string()
    }
}

/// `![name](dest)` for an image, `[name](dest)` otherwise.
pub fn link_to(name: &str, dest: &str) -> String {
    let bang = if is_image(dest) { "!" } else { "" };
    format!("{bang}[{name}]({})", link_dest(dest))
}

/// `path` relative to the folder `dir`, with `/` separators; absolute when there's no
/// common root (another drive).
pub fn rel_path(dir: &std::path::Path, path: &std::path::Path) -> String {
    use std::path::Component;
    let a: Vec<Component> = dir.components().collect();
    let b: Vec<Component> = path.components().collect();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    // nothing in common, or a different Windows drive: absolute
    if common == 0 || matches!(b.first(), Some(Component::Prefix(_))) && common < 2 {
        return path.to_string_lossy().replace('\\', "/");
    }
    let mut parts: Vec<String> = a[common..].iter().map(|_| "..".to_string()).collect();
    parts.extend(b[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    parts.join("/")
}

/// Paths from pasted `file://` URIs, one per line (how Linux file managers copy files).
pub fn file_uris(text: &str) -> Option<Vec<std::path::PathBuf>> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    if lines.is_empty() {
        return None;
    }
    lines
        .iter()
        .map(|l| {
            let p = l.strip_prefix("file://")?;
            let p = percent_decode(p);
            // file:///C:/x on Windows
            let p = if p.len() > 2 && p.as_bytes()[0] == b'/' && p.as_bytes()[2] == b':' { p[1..].to_string() } else { p };
            Some(std::path::PathBuf::from(p))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (b[i], b.get(i + 1).and_then(|c| hex(*c)), b.get(i + 2).and_then(|c| hex(*c))) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_helpers() {
        assert_eq!(as_url(" https://example.com/a?b=1 \n"), Some("https://example.com/a?b=1"));
        assert_eq!(as_url("www.rust-lang.org"), Some("www.rust-lang.org"));
        assert_eq!(as_url("https://a.com b"), None);
        assert_eq!(as_url("see https://a.com"), None);
        assert!(is_image("https://x.org/cat.PNG?size=2"));
        assert!(!is_image("https://x.org/cat.html"));
        assert_eq!(link_to("cat", "images/cat 1.png"), "![cat](<images/cat 1.png>)");
        assert_eq!(link_to("doc", "doc.pdf"), "[doc](doc.pdf)");
        let p = std::path::Path::new;
        if cfg!(windows) {
            assert_eq!(rel_path(p(r"C:\notes"), p(r"C:\notes\img\a.png")), "img/a.png");
            assert_eq!(rel_path(p(r"C:\notes\sub"), p(r"C:\pics\a.png")), "../../pics/a.png");
            assert_eq!(rel_path(p(r"C:\notes"), p(r"D:\a.png")), "D:/a.png");
        } else {
            assert_eq!(rel_path(p("/home/u/notes"), p("/home/u/notes/img/a.png")), "img/a.png");
            assert_eq!(rel_path(p("/home/u/notes/sub"), p("/home/u/pics/a.png")), "../../pics/a.png");
        }
        assert_eq!(file_uris("file:///home/u/my%20cat.png\nfile:///tmp/b.txt"), Some(vec!["/home/u/my cat.png".into(), "/tmp/b.txt".into()]));
        assert_eq!(file_uris("file:///C:/x/a.png"), Some(vec!["C:/x/a.png".into()]));
        assert_eq!(file_uris("hello"), None);
    }

    fn next(line: &str) -> Option<(usize, String)> {
        marker(line).map(|m| (m.len, m.next))
    }

    #[test]
    fn list_markers() {
        assert_eq!(next("- item"), Some((2, "- ".into())));
        assert_eq!(next("  * item"), Some((4, "  * ".into())));
        assert_eq!(next("9. nine"), Some((3, "10. ".into())));
        assert_eq!(next("3) three"), Some((3, "4) ".into())));
        assert_eq!(next("- [x] done"), Some((6, "- [ ] ".into())));
        assert_eq!(next("- [ ]"), Some((5, "- [ ] ".into())));
        assert_eq!(next("> quoted"), Some((2, "> ".into())));
        assert_eq!(next(">> deeper"), Some((3, ">> ".into())));
        assert_eq!(next("> - in quote"), Some((4, "> - ".into())));
        assert_eq!(next("- "), Some((2, "- ".into())));
        assert_eq!(next("-item"), None);
        assert_eq!(next("plain text"), None);
        assert_eq!(next("2024.01 report"), None);
        assert_eq!(next("* * *"), None); // horizontal rule
        assert_eq!(next("- - -"), None);
    }

    #[test]
    fn outline_headings() {
        let text = "# Title\n\nintro\n## Part one ##\n```sh\n# not a heading\n```\nSetext\n======\n\
                    Sub\n---\n- item\n---\n#nospace\n####### seven\n    # indented code\n### \n~~~\n## in tilde\n~~~\n## Last";
        let got: Vec<(usize, usize, String)> = headings(text.lines()).into_iter().map(|h| (h.line, h.level, h.title)).collect();
        assert_eq!(
            got,
            vec![
                (0, 1, "Title".into()),
                (3, 2, "Part one".into()),
                (7, 1, "Setext".into()),
                (9, 2, "Sub".into()),
                (16, 3, "".into()),
                (20, 2, "Last".into()),
            ]
        );
    }

    #[test]
    fn emphasis_toggles() {
        assert_eq!(toggle_wrap("word", "**"), "**word**");
        assert_eq!(toggle_wrap("**word**", "**"), "word");
        assert_eq!(toggle_wrap("*word*", "*"), "word");
        assert_eq!(toggle_wrap("**word**", "*"), "***word***");
        assert_eq!(toggle_wrap("", "**"), "****");
    }
}
