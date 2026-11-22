//! Multi-selection model. Positions are char indices into the rope.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sel {
    pub anchor: usize,
    pub head: usize,
    /// Remembered visual column for vertical movement.
    pub goal: Option<usize>,
}

impl Sel {
    pub fn caret(p: usize) -> Self {
        Self { anchor: p, head: p, goal: None }
    }
    pub fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head, goal: None }
    }
    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }
    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
    pub fn reversed(&self) -> bool {
        self.head < self.anchor
    }
    pub fn same_range(&self, o: &Sel) -> bool {
        self.anchor == o.anchor && self.head == o.head
    }
}

pub fn same_ranges(a: &[Sel], b: &[Sel]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.same_range(y))
}

/// Sort selections, clamp them to the document and merge overlapping ones.
/// Keeps `primary` pointing at the selection that contains the old primary.
pub fn normalize(sels: &mut Vec<Sel>, primary: &mut usize, len: usize) {
    if sels.is_empty() {
        sels.push(Sel::caret(0));
        *primary = 0;
    }
    for s in sels.iter_mut() {
        s.anchor = s.anchor.min(len);
        s.head = s.head.min(len);
    }
    let prim = sels[(*primary).min(sels.len() - 1)];
    sels.sort_by_key(|s| (s.start(), s.end()));
    let mut out: Vec<Sel> = Vec::with_capacity(sels.len());
    let mut prim_idx = 0;
    for s in sels.drain(..) {
        if let Some(last) = out.last_mut() {
            let overlap = s.start() < last.end()
                || (s.start() == last.end() && (s.is_empty() || last.is_empty()));
            if overlap {
                let (st, en) = (last.start(), last.end().max(s.end()));
                *last = if last.reversed() { Sel::new(en, st) } else { Sel::new(st, en) };
                if s.same_range(&prim) {
                    prim_idx = out.len() - 1;
                }
                continue;
            }
        }
        if s.same_range(&prim) {
            prim_idx = out.len();
        }
        out.push(s);
    }
    *sels = out;
    *primary = prim_idx.min(sels.len() - 1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_overlaps_and_duplicates() {
        let mut v = vec![Sel::caret(5), Sel::new(0, 3), Sel::caret(3), Sel::caret(5), Sel::new(4, 8)];
        let mut p = 4;
        normalize(&mut v, &mut p, 100);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].start(), v[0].end()), (0, 3));
        assert_eq!((v[1].start(), v[1].end()), (4, 8));
        assert_eq!(p, 1);
    }
}
