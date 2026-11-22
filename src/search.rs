use regex::RegexBuilder;
use ropey::Rope;

/// Find all matches in `hay` (which must equal `rope.to_string()`), as char ranges.
pub fn find_all(
    rope: &Rope,
    hay: &str,
    needle: &str,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return vec![];
    }
    let pat = if regex { needle.to_string() } else { regex::escape(needle) };
    let pat = if whole_word { format!(r"\b(?:{pat})\b") } else { pat };
    let Ok(re) = RegexBuilder::new(&pat)
        .case_insensitive(!case_sensitive)
        .multi_line(true)
        .build()
    else {
        return vec![];
    };
    re.find_iter(hay)
        .filter(|m| m.end() > m.start())
        .take(200_000)
        .map(|m| (rope.byte_to_char(m.start()), rope.byte_to_char(m.end())))
        .collect()
}

/// Simple fuzzy matcher for the command palette / goto anything.
/// Returns (score, matched char indices) or None if `pattern` isn't a subsequence.
pub fn fuzzy(pattern: &str, cand: &str) -> Option<(i64, Vec<usize>)> {
    let p: Vec<char> = pattern
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect();
    if p.is_empty() {
        return Some((0, vec![]));
    }
    let c: Vec<char> = cand.chars().collect();
    let mut idx = Vec::with_capacity(p.len());
    let mut pi = 0;
    let mut score = 0i64;
    let mut last: Option<usize> = None;
    for (i, &ch) in c.iter().enumerate() {
        if pi < p.len() && ch.to_lowercase().next() == Some(p[pi]) {
            let mut s = 1;
            if last.is_some_and(|l| l + 1 == i) {
                s += 6;
            }
            if i == 0
                || matches!(c[i - 1], '/' | '\\' | '_' | '-' | '.' | ' ' | ':')
                || (c[i - 1].is_lowercase() && ch.is_uppercase())
            {
                s += 8;
            }
            score += s;
            idx.push(i);
            last = Some(i);
            pi += 1;
        }
    }
    if pi < p.len() {
        return None;
    }
    Some((score - c.len() as i64 / 8, idx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_char_ranges() {
        let r = Rope::from_str("zażółć foo Foo foobar");
        let h = r.to_string();
        assert_eq!(find_all(&r, &h, "foo", true, false, false), vec![(7, 10), (15, 18)]);
        assert_eq!(find_all(&r, &h, "foo", false, true, false), vec![(7, 10), (11, 14)]);
    }

    #[test]
    fn fuzzy_matches() {
        assert!(fuzzy("tglcmt", "Toggle Comment").is_some());
        assert!(fuzzy("xyz", "Toggle Comment").is_none());
        let a = fuzzy("main", "src/main.rs").unwrap().0;
        let b = fuzzy("main", "src/my_animation.rs").unwrap().0;
        assert!(a > b);
    }
}
