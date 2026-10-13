//! A line-then-character difference (TL-85: a host SHOULD record a minimal line-then-character difference, so
//! identities of unchanged text survive). Lines are compared by an LCS over the region between the common
//! prefix and suffix; when that region is too large, it is one replacement.

/// One edit against the old text, in characters (Unicode scalar values): at `pos`, delete `delete`
/// characters, insert `insert`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    /// Position in the old text.
    pub pos: usize,
    /// Characters deleted.
    pub delete: usize,
    /// Text inserted.
    pub insert: String,
}

/// Above this many cells, the middle region is one replacement.
const LCS_LIMIT: usize = 4_000_000;

/// Splits after every `\n`, keeping it.
pub fn lines(s: &str) -> Vec<&str> {
    s.split_inclusive('\n').collect()
}

/// A line hunk: `old[a..b]` replaced by `new[c..d]` (line indexes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineHunk {
    /// First old line.
    pub a: usize,
    /// End of old lines.
    pub b: usize,
    /// First new line.
    pub c: usize,
    /// End of new lines.
    pub d: usize,
}

/// The line hunks turning `old` into `new`, in order.
pub fn line_hunks(old: &[&str], new: &[&str]) -> Vec<LineHunk> {
    let mut pre = 0;
    while pre < old.len() && pre < new.len() && old[pre] == new[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < old.len() - pre
        && suf < new.len() - pre
        && old[old.len() - 1 - suf] == new[new.len() - 1 - suf]
    {
        suf += 1;
    }
    let (o, n) = (&old[pre..old.len() - suf], &new[pre..new.len() - suf]);
    if o.is_empty() && n.is_empty() {
        return Vec::new();
    }
    if o.is_empty() || n.is_empty() || o.len().saturating_mul(n.len()) > LCS_LIMIT {
        return vec![LineHunk {
            a: pre,
            b: pre + o.len(),
            c: pre,
            d: pre + n.len(),
        }];
    }
    // LCS table, suffix form.
    let (w, h) = (n.len() + 1, o.len() + 1);
    let mut t = vec![0u32; w * h];
    for i in (0..o.len()).rev() {
        for j in (0..n.len()).rev() {
            t[i * w + j] = if o[i] == n[j] {
                t[(i + 1) * w + j + 1] + 1
            } else {
                t[(i + 1) * w + j].max(t[i * w + j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    let mut open: Option<(usize, usize)> = None;
    while i < o.len() || j < n.len() {
        if i < o.len() && j < n.len() && o[i] == n[j] {
            if let Some((a, c)) = open.take() {
                out.push(LineHunk {
                    a: pre + a,
                    b: pre + i,
                    c: pre + c,
                    d: pre + j,
                });
            }
            i += 1;
            j += 1;
            continue;
        }
        if open.is_none() {
            open = Some((i, j));
        }
        if j < n.len() && (i == o.len() || t[i * w + j + 1] >= t[(i + 1) * w + j]) {
            j += 1;
        } else {
            i += 1;
        }
    }
    if let Some((a, c)) = open {
        out.push(LineHunk {
            a: pre + a,
            b: pre + o.len(),
            c: pre + c,
            d: pre + n.len(),
        });
    }
    out
}

/// The character edits turning `old` into `new`: line hunks, each narrowed to its changed characters.
pub fn char_edits(old: &str, new: &str) -> Vec<Edit> {
    let ol = lines(old);
    let nl = lines(new);
    // Character offsets of line starts in the old text.
    let mut starts = Vec::with_capacity(ol.len() + 1);
    let mut acc = 0;
    for l in &ol {
        starts.push(acc);
        acc += l.chars().count();
    }
    starts.push(acc);
    let mut out = Vec::new();
    for h in line_hunks(&ol, &nl) {
        let o: String = ol[h.a..h.b].concat();
        let n: String = nl[h.c..h.d].concat();
        let oc: Vec<char> = o.chars().collect();
        let nc: Vec<char> = n.chars().collect();
        let mut p = 0;
        while p < oc.len() && p < nc.len() && oc[p] == nc[p] {
            p += 1;
        }
        let mut s = 0;
        while s < oc.len() - p && s < nc.len() - p && oc[oc.len() - 1 - s] == nc[nc.len() - 1 - s] {
            s += 1;
        }
        let delete = oc.len() - p - s;
        let insert: String = nc[p..nc.len() - s].iter().collect();
        if delete == 0 && insert.is_empty() {
            continue;
        }
        out.push(Edit {
            pos: starts[h.a] + p,
            delete,
            insert,
        });
    }
    out
}

/// Applies edits (positions against the original, in order) to a text.
pub fn apply(old: &str, edits: &[Edit]) -> String {
    let chars: Vec<char> = old.chars().collect();
    let mut out = String::with_capacity(old.len());
    let mut at = 0;
    for e in edits {
        out.extend(&chars[at..e.pos]);
        out.push_str(&e.insert);
        at = e.pos + e.delete;
    }
    out.extend(&chars[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_reproduce_the_new_text() {
        let cases = [
            ("", ""),
            ("", "a\nb\n"),
            ("a\nb\n", ""),
            ("a\nb\nc\n", "a\nB\nc\n"),
            ("a\nb\nc\nd\ne\n", "x\na\nc\nd\ne\nf\n"),
            ("one two\nthree\n", "one too\nthree\nfour"),
            ("é🙂x\n", "é🙂y\n"),
            ("no newline", "no newline at all"),
        ];
        for (o, n) in cases {
            let e = char_edits(o, n);
            assert_eq!(apply(o, &e), n, "{o:?} -> {n:?}: {e:?}");
        }
    }

    #[test]
    fn unchanged_lines_are_kept() {
        let e = char_edits("a\nb\nc\nd\n", "a\nb2\nc\nd2\n");
        assert_eq!(e.len(), 2);
        assert_eq!(
            e[0],
            Edit {
                pos: 3,
                delete: 0,
                insert: "2".into()
            }
        );
    }
}
