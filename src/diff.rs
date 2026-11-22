//! Line + word diff between two ropes (the engine behind the side-by-side diff view).

use ropey::Rope;
use similar::{capture_diff_slices, Algorithm, DiffOp, TextDiff};
use std::ops::Range;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Lines only on the right.
    Insert,
    /// Lines only on the left.
    Delete,
    /// Lines changed on both sides.
    Replace,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hunk {
    pub kind: Kind,
    /// Line ranges (end exclusive) on each side.
    pub left: Range<usize>,
    pub right: Range<usize>,
    /// Changed words inside a Replace hunk, as absolute char ranges in each document.
    pub left_words: Vec<Range<usize>>,
    pub right_words: Vec<Range<usize>>,
}

/// Char offset of the start of `line`, or the document end past the last line.
pub fn line_pos(rope: &Rope, line: usize) -> usize {
    if line >= rope.len_lines() {
        rope.len_chars()
    } else {
        rope.line_to_char(line)
    }
}

pub fn compute(left: &Rope, right: &Rope) -> Vec<Hunk> {
    let a = left.to_string();
    let b = right.to_string();
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Myers)
        .timeout(Duration::from_secs(2))
        .diff_lines(&a, &b);
    let mut hunks: Vec<Hunk> = vec![];
    for op in diff.ops() {
        let (l, r) = match *op {
            DiffOp::Equal { .. } => continue,
            DiffOp::Delete { old_index, old_len, new_index } => (old_index..old_index + old_len, new_index..new_index),
            DiffOp::Insert { old_index, new_index, new_len } => (old_index..old_index, new_index..new_index + new_len),
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                (old_index..old_index + old_len, new_index..new_index + new_len)
            }
        };
        // adjacent delete + insert = one modified block
        if let Some(last) = hunks.last_mut() {
            if last.left.end == l.start && last.right.end == r.start {
                last.left.end = l.end;
                last.right.end = r.end;
                continue;
            }
        }
        hunks.push(Hunk { kind: Kind::Replace, left: l, right: r, left_words: vec![], right_words: vec![] });
    }
    for h in &mut hunks {
        h.kind = if h.left.is_empty() {
            Kind::Insert
        } else if h.right.is_empty() {
            Kind::Delete
        } else {
            Kind::Replace
        };
        if h.kind == Kind::Replace {
            inline_words(left, right, h);
        }
    }
    hunks
}

/// Split into word runs, whitespace runs and single punctuation chars.
fn tokens(s: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut start = 0;
    let mut prev: Option<u8> = None;
    for (i, c) in s.char_indices() {
        let cls = if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        };
        if let Some(p) = prev {
            if p != cls || cls == 2 {
                out.push(&s[start..i]);
                start = i;
            }
        }
        prev = Some(cls);
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

fn push_range(v: &mut Vec<Range<usize>>, r: Range<usize>) {
    if r.is_empty() {
        return;
    }
    if let Some(last) = v.last_mut() {
        if last.end == r.start {
            last.end = r.end;
            return;
        }
    }
    v.push(r);
}

fn inline_words(left: &Rope, right: &Rope, h: &mut Hunk) {
    let (ls, le) = (line_pos(left, h.left.start), line_pos(left, h.left.end));
    let (rs, re) = (line_pos(right, h.right.start), line_pos(right, h.right.end));
    if le - ls > 50_000 || re - rs > 50_000 {
        return; // too big: whole-line highlighting only
    }
    let a = left.slice(ls..le).to_string();
    let b = right.slice(rs..re).to_string();
    let (ta, tb) = (tokens(&a), tokens(&b));
    let prefix = |t: &[&str]| {
        let mut p = vec![0usize];
        for s in t {
            p.push(p.last().unwrap() + s.chars().count());
        }
        p
    };
    let (pa, pb) = (prefix(&ta), prefix(&tb));
    for op in capture_diff_slices(Algorithm::Myers, &ta, &tb) {
        match op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete { old_index, old_len, .. } => {
                push_range(&mut h.left_words, ls + pa[old_index]..ls + pa[old_index + old_len])
            }
            DiffOp::Insert { new_index, new_len, .. } => {
                push_range(&mut h.right_words, rs + pb[new_index]..rs + pb[new_index + new_len])
            }
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                push_range(&mut h.left_words, ls + pa[old_index]..ls + pa[old_index + old_len]);
                push_range(&mut h.right_words, rs + pb[new_index]..rs + pb[new_index + new_len]);
            }
        }
    }
}

/// Map a (fractional) line on one side to the corresponding line on the other side, so
/// both panes scroll in step. `from_left` maps left -> right, otherwise right -> left.
pub fn map_line(hunks: &[Hunk], x: f32, from_left: bool) -> f32 {
    let mut delta = 0.0;
    for h in hunks {
        let (f, t) = if from_left { (&h.left, &h.right) } else { (&h.right, &h.left) };
        let (fs, fe, ts, te) = (f.start as f32, f.end as f32, t.start as f32, t.end as f32);
        if x < fs {
            return x + (ts - fs);
        }
        if x < fe {
            return ts + (x - fs) / (fe - fs) * (te - ts);
        }
        delta = te - fe;
    }
    x + delta
}

/// Index of the next/previous hunk relative to `line` on the given side.
pub fn neighbour(hunks: &[Hunk], line: usize, right_side: bool, forward: bool) -> Option<usize> {
    let start = |h: &Hunk| if right_side { h.right.start } else { h.left.start };
    if forward {
        hunks.iter().position(|h| start(h) > line).or(if hunks.is_empty() { None } else { Some(0) })
    } else {
        hunks.iter().rposition(|h| start(h) < line).or(hunks.len().checked_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_and_word_hunks() {
        let l = Rope::from_str("a\nfoo(bar)\nc\n");
        let r = Rope::from_str("a\nfoo(baz)\nc\nd\n");
        let h = compute(&l, &r);
        assert_eq!(h.len(), 2);
        assert_eq!((h[0].kind, h[0].left.clone(), h[0].right.clone()), (Kind::Replace, 1..2, 1..2));
        // only the changed word is highlighted: "bar" / "baz" at chars 6..9
        assert_eq!(h[0].left_words, vec![6..9]);
        assert_eq!(h[0].right_words, vec![6..9]);
        assert_eq!((h[1].kind, h[1].left.clone(), h[1].right.clone()), (Kind::Insert, 3..3, 3..4));
    }

    #[test]
    fn delete_hunk() {
        let h = compute(&Rope::from_str("a\nb\nc\n"), &Rope::from_str("a\nc\n"));
        assert_eq!(h.len(), 1);
        assert_eq!((h[0].kind, h[0].left.clone(), h[0].right.clone()), (Kind::Delete, 1..2, 1..1));
    }

    #[test]
    fn identical_has_no_hunks() {
        let r = Rope::from_str("same\ntext\n");
        assert!(compute(&r, &r.clone()).is_empty());
    }

    #[test]
    fn scroll_mapping_skips_over_inserted_block() {
        // three lines inserted on the right after line 0
        let h = vec![Hunk { kind: Kind::Insert, left: 1..1, right: 1..4, left_words: vec![], right_words: vec![] }];
        assert_eq!(map_line(&h, 0.5, false), 0.5);
        assert_eq!(map_line(&h, 2.0, false), 1.0); // inside the insertion -> pinned
        assert_eq!(map_line(&h, 5.0, false), 2.0);
        assert_eq!(map_line(&h, 1.0, true), 4.0);
        assert_eq!(map_line(&h, 2.0, true), 5.0);
    }

    #[test]
    fn navigation_wraps() {
        let h = compute(&Rope::from_str("a\nb\nc\nd\n"), &Rope::from_str("a\nB\nc\nD\n"));
        assert_eq!(neighbour(&h, 0, true, true), Some(0));
        assert_eq!(neighbour(&h, 1, true, true), Some(1));
        assert_eq!(neighbour(&h, 3, true, true), Some(0));
        assert_eq!(neighbour(&h, 0, true, false), Some(1));
    }
}
