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
    fn emphasis_toggles() {
        assert_eq!(toggle_wrap("word", "**"), "**word**");
        assert_eq!(toggle_wrap("**word**", "**"), "word");
        assert_eq!(toggle_wrap("*word*", "*"), "word");
        assert_eq!(toggle_wrap("**word**", "*"), "***word***");
        assert_eq!(toggle_wrap("", "**"), "****");
    }
}
