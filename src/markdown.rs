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

#[cfg(test)]
mod tests {
    use super::*;

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
