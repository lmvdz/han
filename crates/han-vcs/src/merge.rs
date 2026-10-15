//! Pure parts of the git backend's merge handling: reading `git merge-tree --write-tree -z` and finding the
//! regions of a text file where two sides disagree.

use std::collections::BTreeMap;

use crate::ConflictSide;

/// What `git merge-tree --write-tree -z` printed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct MergeTreeOut {
    /// The merged tree (with the conflicted paths as git left them).
    pub tree: String,
    /// Conflicted entries by path: the merge base's, ours and theirs (stages 1 to 3).
    pub stages: BTreeMap<String, [Option<ConflictSide>; 3]>,
    /// The messages: the paths they are about and their type (`CONFLICT (contents)`, ...).
    pub messages: Vec<(Vec<String>, String)>,
}

/// Reads the output of `git merge-tree --write-tree -z` (with messages). Returns `None` for output that
/// is not in that shape.
pub(crate) fn parse_merge_tree(out: &[u8]) -> Option<MergeTreeOut> {
    let tokens: Vec<&[u8]> = out.split(|b| *b == 0).collect();
    let text = |i: usize| {
        tokens
            .get(i)
            .map(|t| String::from_utf8_lossy(t).into_owned())
    };
    let mut res = MergeTreeOut {
        tree: text(0).filter(|t| !t.is_empty())?,
        ..MergeTreeOut::default()
    };
    let mut i = 1;
    // `mode SP id SP stage TAB path`, until an empty token.
    while let Some(t) = text(i).filter(|t| !t.is_empty()) {
        i += 1;
        let (meta, path) = t.split_once('\t')?;
        let mut f = meta.split(' ');
        let (mode, id, stage) = (f.next()?, f.next()?, f.next()?);
        let stage: usize = stage.parse().ok().filter(|s| (1..=3).contains(s))?;
        res.stages.entry(path.to_owned()).or_default()[stage - 1] = Some(ConflictSide {
            mode: mode.to_owned(),
            id: id.to_owned(),
        });
    }
    i += 1;
    // `count NUL path NUL ... type NUL message NUL`, repeated.
    while let Some(count) = text(i).filter(|t| !t.is_empty()) {
        let n: usize = count.parse().ok()?;
        let paths = (0..n)
            .map(|k| text(i + 1 + k))
            .collect::<Option<Vec<_>>>()?;
        let kind = text(i + 1 + n)?;
        res.messages.push((paths, kind));
        i += n + 3;
    }
    Some(res)
}

/// A changed region of a `-U0` diff, in 0-based lines of the old file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Change {
    /// The first old line it replaces (for a pure insertion, the line it goes before).
    pub start: usize,
    /// How many old lines it replaces.
    pub old_len: usize,
    /// How many new lines replace them.
    pub new_len: usize,
}

/// The regions of a `git diff -U0` output.
pub(crate) fn parse_changes(diff: &str) -> Vec<Change> {
    fn range(tok: &str) -> Option<(usize, usize)> {
        let tok = tok.get(1..)?;
        match tok.split_once(',') {
            Some((s, l)) => Some((s.parse().ok()?, l.parse().ok()?)),
            None => Some((tok.parse().ok()?, 1)),
        }
    }
    diff.lines()
        .filter_map(|l| l.strip_prefix("@@ "))
        .filter_map(|rest| {
            let mut t = rest.split(' ');
            let (os, ol) = range(t.next()?)?;
            let (_, nl) = range(t.next()?)?;
            Some(Change {
                start: if ol == 0 { os } else { os.saturating_sub(1) },
                old_len: ol,
                new_len: nl,
            })
        })
        .collect()
}

/// A region where both sides changed the same base lines differently: line ranges `(start, len)`, 0-based,
/// in the base, ours and theirs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    pub base: (usize, usize),
    pub ours: (usize, usize),
    pub theirs: (usize, usize),
}

/// The disagreements between two sides that changed the same base, given each side's changes against it
/// and the sides' lines. Changes that overlap or touch (in the base) form one region, as in git's own
/// line merge; a region is a disagreement when both sides are in it and their lines differ. `None` when
/// the changes do not fit the files.
pub(crate) fn regions(
    ours_changes: &[Change],
    theirs_changes: &[Change],
    ours: &[&[u8]],
    theirs: &[&[u8]],
) -> Option<Vec<Region>> {
    let mut all: Vec<(Change, bool)> = ours_changes
        .iter()
        .map(|c| (*c, true))
        .chain(theirs_changes.iter().map(|c| (*c, false)))
        .collect();
    all.sort_by_key(|(c, o)| (c.start, c.start + c.old_len, !*o));
    let mut out = Vec::new();
    // The lines each side has gained or lost before the current region.
    let (mut off_o, mut off_t) = (0isize, 0isize);
    let mut i = 0;
    while i < all.len() {
        let lo = all[i].0.start;
        let mut hi = lo;
        let mut first = true;
        let (mut d_o, mut d_t) = (0isize, 0isize);
        let (mut has_o, mut has_t) = (false, false);
        while i < all.len() && (first || all[i].0.start <= hi) {
            first = false;
            let (c, is_ours) = all[i];
            hi = hi.max(c.start + c.old_len);
            let d = c.new_len as isize - c.old_len as isize;
            if is_ours {
                d_o += d;
                has_o = true;
            } else {
                d_t += d;
                has_t = true;
            }
            i += 1;
        }
        if has_o && has_t {
            let span = isize::try_from(hi - lo).ok()?;
            let o = (
                usize::try_from(lo as isize + off_o).ok()?,
                usize::try_from(span + d_o).ok()?,
            );
            let t = (
                usize::try_from(lo as isize + off_t).ok()?,
                usize::try_from(span + d_t).ok()?,
            );
            let o_lines = ours.get(o.0..o.0 + o.1)?;
            let t_lines = theirs.get(t.0..t.0 + t.1)?;
            if o_lines != t_lines {
                out.push(Region {
                    base: (lo, hi - lo),
                    ours: o,
                    theirs: t,
                });
            }
        }
        off_o += d_o;
        off_t += d_t;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<&[u8]> {
        s.as_bytes().split_inclusive(|b| *b == b'\n').collect()
    }

    #[test]
    fn diff_headers_become_changes() {
        let d = "diff --git a/x b/x\n@@ -2 +2,2 @@\n-b\n+B\n+B2\n@@ -5,0 +7 @@\n+n\n@@ -9,2 +10,0 @@\n-x\n-y\n";
        assert_eq!(
            parse_changes(d),
            vec![
                Change {
                    start: 1,
                    old_len: 1,
                    new_len: 2
                },
                Change {
                    start: 5,
                    old_len: 0,
                    new_len: 1
                },
                Change {
                    start: 8,
                    old_len: 2,
                    new_len: 0
                },
            ]
        );
    }

    #[test]
    fn same_line_changed_differently_is_one_region() {
        let ours = lines("a\nO\nc\nd\n");
        let theirs = lines("a\nT\nc\nd\n");
        let r = regions(
            &[Change {
                start: 1,
                old_len: 1,
                new_len: 1,
            }],
            &[Change {
                start: 1,
                old_len: 1,
                new_len: 1,
            }],
            &ours,
            &theirs,
        );
        assert_eq!(
            r,
            Some(vec![Region {
                base: (1, 1),
                ours: (1, 1),
                theirs: (1, 1)
            }])
        );
    }

    #[test]
    fn apart_changes_and_equal_changes_are_clean() {
        let ours = lines("A\nb\nc\nd\n");
        let theirs = lines("a\nb\nc\nD\n");
        let apart = regions(
            &[Change {
                start: 0,
                old_len: 1,
                new_len: 1,
            }],
            &[Change {
                start: 3,
                old_len: 1,
                new_len: 1,
            }],
            &ours,
            &theirs,
        );
        assert_eq!(apart, Some(vec![]));
        let same = lines("a\nX\n");
        let r = regions(
            &[Change {
                start: 1,
                old_len: 1,
                new_len: 1,
            }],
            &[Change {
                start: 1,
                old_len: 1,
                new_len: 1,
            }],
            &same,
            &same,
        );
        assert_eq!(r, Some(vec![]));
    }

    #[test]
    fn later_regions_are_placed_by_what_each_side_gained_before() {
        // base: 1 2 3 4 5 6; ours inserts two lines before 2 and edits 6; theirs deletes 1 and edits 6.
        let ours = lines("1\nx\ny\n2\n3\n4\n5\nO\n");
        let theirs = lines("2\n3\n4\n5\nT\n");
        let r = regions(
            &[
                Change {
                    start: 1,
                    old_len: 0,
                    new_len: 2,
                },
                Change {
                    start: 5,
                    old_len: 1,
                    new_len: 1,
                },
            ],
            &[
                Change {
                    start: 0,
                    old_len: 1,
                    new_len: 0,
                },
                Change {
                    start: 5,
                    old_len: 1,
                    new_len: 1,
                },
            ],
            &ours,
            &theirs,
        )
        .unwrap();
        // the insertion before line 2 touches theirs' deletion of line 1: that region differs too
        assert_eq!(r.len(), 2, "{r:?}");
        assert_eq!(
            r[1],
            Region {
                base: (5, 1),
                ours: (7, 1),
                theirs: (4, 1)
            }
        );
    }

    #[test]
    fn merge_tree_output_is_read() {
        let mut out = Vec::new();
        out.extend_from_slice(b"abc\x00100644 11 1\tf\x00100644 22 2\tf\x00100644 33 3\tf\x00\x00");
        out.extend_from_slice(b"1\0f\0Auto-merging\0Auto-merging f\n\0");
        out.extend_from_slice(b"2\0dd~ours\0dd\0CONFLICT (file/directory)\0msg\n\0");
        let p = parse_merge_tree(&out).unwrap();
        assert_eq!(p.tree, "abc");
        let f = &p.stages["f"];
        assert!(f.iter().all(Option::is_some));
        assert_eq!(f[2].as_ref().unwrap().id, "33");
        assert_eq!(p.messages.len(), 2);
        assert_eq!(p.messages[1].0, vec!["dd~ours", "dd"]);
        assert_eq!(p.messages[1].1, "CONFLICT (file/directory)");
        let clean = parse_merge_tree(b"abc\0").unwrap();
        assert!(clean.stages.is_empty() && clean.messages.is_empty());
    }
}
