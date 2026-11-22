//! Rope helpers: line/column math, word & camel-hump boundaries, brackets, quotes.

use ropey::Rope;
use std::ops::Range;

// ---------------------------------------------------------------- lines / columns

/// Number of chars in a line, excluding the trailing newline.
pub fn line_len(rope: &Rope, line: usize) -> usize {
    let s = rope.line(line);
    let n = s.len_chars();
    if n > 0 && s.char(n - 1) == '\n' {
        n - 1
    } else {
        n
    }
}

pub fn line_end(rope: &Rope, line: usize) -> usize {
    rope.line_to_char(line) + line_len(rope, line)
}

pub fn line_text(rope: &Rope, line: usize) -> String {
    rope.line(line).chars().take(line_len(rope, line)).collect()
}

pub fn pos_to_lc(rope: &Rope, pos: usize) -> (usize, usize) {
    let pos = pos.min(rope.len_chars());
    let l = rope.char_to_line(pos);
    (l, pos - rope.line_to_char(l))
}

pub fn lc_to_pos(rope: &Rope, line: usize, col: usize) -> usize {
    let line = line.min(rope.len_lines() - 1);
    rope.line_to_char(line) + col.min(line_len(rope, line))
}

pub fn first_non_ws(rope: &Rope, line: usize) -> usize {
    rope.line(line)
        .chars()
        .take(line_len(rope, line))
        .take_while(|c| *c == ' ' || *c == '\t')
        .count()
}

pub fn leading_ws(rope: &Rope, line: usize) -> String {
    rope.line(line)
        .chars()
        .take(line_len(rope, line))
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Char column -> visual column (tabs expanded).
pub fn visual_col(rope: &Rope, line: usize, col: usize, tab: usize) -> usize {
    let mut v = 0;
    for (i, c) in rope.line(line).chars().enumerate() {
        if i >= col || c == '\n' {
            break;
        }
        v += if c == '\t' { tab - v % tab } else { 1 };
    }
    v
}

/// Visual column -> char column (nearest char boundary).
pub fn col_from_visual(rope: &Rope, line: usize, vcol: usize, tab: usize) -> usize {
    let len = line_len(rope, line);
    let mut v = 0;
    for (i, c) in rope.line(line).chars().enumerate() {
        if i >= len {
            return len;
        }
        let w = if c == '\t' { tab - v % tab } else { 1 };
        if vcol < v + w {
            return if w > 1 && (vcol - v) * 2 >= w { i + 1 } else { i };
        }
        v += w;
    }
    len
}

// ---------------------------------------------------------------- words & camel humps

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum Class {
    Space,
    Word,
    Punct,
    Newline,
}

pub fn class(c: char) -> Class {
    if c == '\n' {
        Class::Newline
    } else if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

pub fn is_word_char(c: char) -> bool {
    class(c) == Class::Word
}

/// Is there a camel-hump boundary *before* `cur`?
/// `fooBar` -> foo|Bar, `HTMLParser` -> HTML|Parser, `foo_bar` -> foo|_bar, `utf8` -> utf|8
pub fn hump_boundary(prev: char, cur: char, next: Option<char>) -> bool {
    if cur == '_' {
        return prev != '_';
    }
    if prev == '_' {
        return false;
    }
    if prev.is_lowercase() && cur.is_uppercase() {
        return true;
    }
    if prev.is_uppercase() && cur.is_uppercase() && next.is_some_and(|n| n.is_lowercase()) {
        return true;
    }
    (prev.is_alphabetic() && cur.is_numeric()) || (prev.is_numeric() && cur.is_alphabetic())
}

/// Ctrl+Right target. With `camel`, stops at camel-hump boundaries inside words.
pub fn next_word(rope: &Rope, pos: usize, camel: bool) -> usize {
    let n = rope.len_chars();
    if pos >= n {
        return n;
    }
    let ch = |i: usize| rope.char(i);
    let mut i = pos;
    if ch(i) == '\n' {
        return i + 1;
    }
    while i < n && class(ch(i)) == Class::Space {
        i += 1;
    }
    if i >= n || ch(i) == '\n' {
        return i;
    }
    let cls = class(ch(i));
    i += 1;
    while i < n && class(ch(i)) == cls {
        if camel && cls == Class::Word {
            let next = if i + 1 < n { Some(ch(i + 1)) } else { None };
            if hump_boundary(ch(i - 1), ch(i), next) {
                break;
            }
        }
        i += 1;
    }
    i
}

/// Ctrl+Left target. With `camel`, stops at camel-hump boundaries inside words.
pub fn prev_word(rope: &Rope, pos: usize, camel: bool) -> usize {
    let n = rope.len_chars();
    let pos = pos.min(n);
    if pos == 0 {
        return 0;
    }
    let ch = |i: usize| rope.char(i);
    let mut i = pos;
    if ch(i - 1) == '\n' {
        return i - 1;
    }
    while i > 0 && class(ch(i - 1)) == Class::Space {
        i -= 1;
    }
    if i == 0 || ch(i - 1) == '\n' {
        return i;
    }
    let cls = class(ch(i - 1));
    i -= 1;
    while i > 0 && class(ch(i - 1)) == cls {
        if camel && cls == Class::Word {
            let next = if i + 1 < n { Some(ch(i + 1)) } else { None };
            if hump_boundary(ch(i - 1), ch(i), next) {
                break;
            }
        }
        i -= 1;
    }
    i
}

/// Whole word at (or touching, on the left) `pos`.
pub fn word_at(rope: &Rope, pos: usize) -> Option<Range<usize>> {
    let n = rope.len_chars();
    let is_w = |i: usize| i < n && is_word_char(rope.char(i));
    let p = if is_w(pos) {
        pos
    } else if pos > 0 && is_w(pos - 1) {
        pos - 1
    } else {
        return None;
    };
    let mut s = p;
    while s > 0 && is_w(s - 1) {
        s -= 1;
    }
    let mut e = p + 1;
    while e < n && is_w(e) {
        e += 1;
    }
    Some(s..e)
}

/// Camel hump containing `pos` (e.g. `Bar` in `fooBarBaz` when the caret is on it).
pub fn hump_at(rope: &Rope, pos: usize) -> Option<Range<usize>> {
    let w = word_at(rope, pos)?;
    let chars: Vec<char> = rope.slice(w.clone()).chars().collect();
    let mut bounds = vec![0];
    for i in 1..chars.len() {
        if hump_boundary(chars[i - 1], chars[i], chars.get(i + 1).copied()) {
            bounds.push(i);
        }
    }
    bounds.push(chars.len());
    let rel = pos.saturating_sub(w.start).min(chars.len());
    for win in bounds.windows(2) {
        if rel >= win[0] && rel < win[1] {
            return Some(w.start + win[0]..w.start + win[1]);
        }
    }
    let k = bounds.len();
    Some(w.start + bounds[k - 2]..w.end)
}

/// Range selected by a double click: a word, or a run of same-class chars.
pub fn double_click_range(rope: &Rope, pos: usize) -> Range<usize> {
    if let Some(w) = word_at(rope, pos) {
        return w;
    }
    let n = rope.len_chars();
    if pos >= n {
        return pos..pos;
    }
    let c = class(rope.char(pos));
    if c == Class::Newline {
        return pos..pos;
    }
    let mut s = pos;
    while s > 0 && class(rope.char(s - 1)) == c {
        s -= 1;
    }
    let mut e = pos + 1;
    while e < n && class(rope.char(e)) == c {
        e += 1;
    }
    s..e
}

// ---------------------------------------------------------------- brackets & quotes

const OPEN: [char; 3] = ['(', '[', '{'];
const CLOSE: [char; 3] = [')', ']', '}'];

fn match_forward(rope: &Rope, open_pos: usize, k: usize, limit: usize) -> Option<usize> {
    let mut d = 0usize;
    for (j, c) in rope.chars_at(open_pos + 1).enumerate().take(limit) {
        if c == OPEN[k] {
            d += 1;
        } else if c == CLOSE[k] {
            if d == 0 {
                return Some(open_pos + 1 + j);
            }
            d -= 1;
        }
    }
    None
}

fn match_backward(rope: &Rope, close_pos: usize, k: usize, limit: usize) -> Option<usize> {
    let mut d = 0usize;
    let mut it = rope.chars_at(close_pos);
    let mut i = close_pos;
    let mut steps = 0;
    while let Some(c) = it.prev() {
        i -= 1;
        steps += 1;
        if steps > limit {
            break;
        }
        if c == CLOSE[k] {
            d += 1;
        } else if c == OPEN[k] {
            if d == 0 {
                return Some(i);
            }
            d -= 1;
        }
    }
    None
}

/// If there's a bracket at `pos` (or just before it), return (bracket, its match).
pub fn matching_bracket(rope: &Rope, pos: usize, limit: usize) -> Option<(usize, usize)> {
    let n = rope.len_chars();
    let try_at = |p: usize| -> Option<(usize, usize)> {
        if p >= n {
            return None;
        }
        let c = rope.char(p);
        if let Some(k) = OPEN.iter().position(|&x| x == c) {
            return match_forward(rope, p, k, limit).map(|m| (p, m));
        }
        if let Some(k) = CLOSE.iter().position(|&x| x == c) {
            return match_backward(rope, p, k, limit).map(|m| (p, m));
        }
        None
    };
    try_at(pos).or_else(|| if pos > 0 { try_at(pos - 1) } else { None })
}

/// Innermost bracket pair (open, close) with open < start and close >= end.
pub fn enclosing_brackets(rope: &Rope, start: usize, end: usize) -> Option<(usize, usize)> {
    const LIMIT: usize = 2_000_000;
    let mut depth = [0usize; 3];
    let mut it = rope.chars_at(start);
    let mut i = start;
    let mut steps = 0;
    while let Some(c) = it.prev() {
        i -= 1;
        steps += 1;
        if steps > LIMIT {
            break;
        }
        if let Some(k) = CLOSE.iter().position(|&x| x == c) {
            depth[k] += 1;
        } else if let Some(k) = OPEN.iter().position(|&x| x == c) {
            if depth[k] > 0 {
                depth[k] -= 1;
            } else if let Some(close) = match_forward(rope, i, k, LIMIT) {
                if close >= end {
                    return Some((i, close));
                }
            }
        }
    }
    None
}

/// Quote pair on the current line strictly enclosing [start, end).
pub fn enclosing_quotes(rope: &Rope, start: usize, end: usize) -> Option<(usize, usize)> {
    let l = rope.char_to_line(start);
    if rope.char_to_line(end) != l {
        return None;
    }
    let ls = rope.line_to_char(l);
    let mut open: Option<(char, usize)> = None;
    let mut esc = false;
    for (i, c) in rope.line(l).chars().enumerate() {
        if esc {
            esc = false;
            continue;
        }
        if c == '\\' {
            esc = true;
            continue;
        }
        if matches!(c, '"' | '\'' | '`') {
            match open {
                None => open = Some((c, i)),
                Some((q, o)) if q == c => {
                    let (a, b) = (ls + o, ls + i);
                    if a < start && b >= end {
                        return Some((a, b));
                    }
                    open = None;
                }
                _ => {}
            }
        }
    }
    None
}

/// Candidate ranges for IntelliJ-style "Extend Selection" (Ctrl+W).
pub fn expand_candidates(rope: &Rope, s: usize, e: usize, camel: bool) -> Vec<Range<usize>> {
    let mut c = Vec::new();
    let len = rope.len_chars();
    if camel {
        if let Some(h) = hump_at(rope, s) {
            c.push(h);
        }
    }
    if let Some(w) = word_at(rope, s) {
        c.push(w);
    }
    if let Some((a, b)) = enclosing_quotes(rope, s, e) {
        c.push(a + 1..b);
        c.push(a..b + 1);
    }
    let l0 = rope.char_to_line(s);
    let l1 = rope.char_to_line(e);
    // trimmed line(s)
    let ts = rope.line_to_char(l0) + first_non_ws(rope, l0);
    let mut te = line_end(rope, l1);
    while te > ts && rope.char(te - 1).is_whitespace() {
        te -= 1;
    }
    if ts < te {
        c.push(ts..te);
    }
    // full lines including newline
    let fe = if l1 + 1 < rope.len_lines() { rope.line_to_char(l1 + 1) } else { len };
    c.push(rope.line_to_char(l0)..fe);
    if let Some((a, b)) = enclosing_brackets(rope, s, e) {
        c.push(a + 1..b);
        let mut is = a + 1;
        while is < b && rope.char(is).is_whitespace() {
            is += 1;
        }
        let mut ie = b;
        while ie > is && rope.char(ie - 1).is_whitespace() {
            ie -= 1;
        }
        c.push(is..ie);
        c.push(a..b + 1);
    }
    c.push(0..len);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stops_right(s: &str) -> Vec<usize> {
        let r = Rope::from_str(s);
        let mut v = vec![];
        let mut p = 0;
        while p < r.len_chars() {
            p = next_word(&r, p, true);
            v.push(p);
        }
        v
    }

    fn stops_left(s: &str) -> Vec<usize> {
        let r = Rope::from_str(s);
        let mut v = vec![];
        let mut p = r.len_chars();
        while p > 0 {
            p = prev_word(&r, p, true);
            v.push(p);
        }
        v
    }

    #[test]
    fn camel_humps_forward() {
        assert_eq!(stops_right("fooBarBaz"), vec![3, 6, 9]);
        assert_eq!(stops_right("HTMLParser"), vec![4, 10]);
        assert_eq!(stops_right("foo_bar"), vec![3, 7]);
        assert_eq!(stops_right("getX y"), vec![3, 4, 6]);
    }

    #[test]
    fn camel_humps_backward() {
        assert_eq!(stops_left("fooBarBaz"), vec![6, 3, 0]);
        assert_eq!(stops_left("HTMLParser"), vec![4, 0]);
        assert_eq!(stops_left("foo_bar"), vec![3, 0]);
    }

    #[test]
    fn no_camel_moves_whole_words() {
        let r = Rope::from_str("fooBar baz");
        assert_eq!(next_word(&r, 0, false), 6);
        assert_eq!(prev_word(&r, 6, false), 0);
    }

    #[test]
    fn hump_and_word() {
        let r = Rope::from_str("let myHttpClient = 1;");
        assert_eq!(hump_at(&r, 8), Some(6..10)); // "Http"
        assert_eq!(word_at(&r, 8), Some(4..16));
    }

    #[test]
    fn brackets() {
        let r = Rope::from_str("f(a, [b, c], d)");
        assert_eq!(enclosing_brackets(&r, 6, 6), Some((5, 10)));
        assert_eq!(enclosing_brackets(&r, 5, 11), Some((1, 14)));
        assert_eq!(matching_bracket(&r, 1, 1000), Some((1, 14)));
        assert_eq!(matching_bracket(&r, 15, 1000), Some((14, 1)));
    }

    #[test]
    fn visual_columns() {
        let r = Rope::from_str("\tab\tc");
        assert_eq!(visual_col(&r, 0, 1, 4), 4);
        assert_eq!(visual_col(&r, 0, 4, 4), 8);
        assert_eq!(col_from_visual(&r, 0, 1, 4), 0);
        assert_eq!(col_from_visual(&r, 0, 3, 4), 1);
        assert_eq!(col_from_visual(&r, 0, 5, 4), 2);
    }
}
