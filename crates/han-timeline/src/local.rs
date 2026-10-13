//! Local edit planning: visible positions to identity ops (TL-47, TL-51, TL-53).

use crate::error::Error;
use crate::history::{At, History};
use crate::ids::{CharId, IdRange, ObjectId};
use crate::op::{Insert, Op};
use crate::store::Store;
use crate::text::{Local, TextObj};

/// CRLF and lone CR become LF (TL-40, TL-47).
pub(crate) fn normalise(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Coalesces characters (in the order given) into identity ranges: consecutive offsets of one
/// insertion form one range.
fn ranges_in_order(t: &TextObj, chars: impl IntoIterator<Item = Local>) -> Vec<IdRange> {
    let mut out: Vec<IdRange> = Vec::new();
    for l in chars {
        let c = t.char_id(l);
        if let Some(last) = out.last_mut()
            && last.insertion == c.insertion
            && last.to == c.offset
        {
            last.to += 1;
            continue;
        }
        out.push(IdRange {
            insertion: c.insertion,
            from: c.offset,
            to: c.offset + 1,
        });
    }
    out
}

/// TL-47 on one text.
pub(crate) fn plan_edit(
    t: &TextObj,
    target: &ObjectId,
    pos: usize,
    delete: usize,
    text: &str,
) -> Result<Option<Op>, Error> {
    let s = normalise(text);
    if t.compact_layout() {
        let len = t.len();
        if pos.checked_add(delete).is_none_or(|end| end > len) {
            return Err(Error::OutOfRange);
        }
        let deletes = t.visible_ranges(pos, pos + delete);
        let inserts = if s.is_empty() {
            Vec::new()
        } else {
            vec![Insert {
                anchor: (pos > 0).then(|| t.char_id(t.visible_char(pos - 1).unwrap())),
                text: s,
            }]
        };
        return Ok(
            (!deletes.is_empty() || !inserts.is_empty()).then(|| Op::TextEdit {
                target: target.clone(),
                deletes,
                inserts,
            }),
        );
    }

    let lay = t.layout();
    let len = lay.visible_index.len();
    if pos.checked_add(delete).is_none_or(|end| end > len) {
        return Err(Error::OutOfRange);
    }
    let doomed = &lay.visible_index[pos..pos + delete];
    // TL-53: no deleting a sealed character, no inserting inside a region.
    if doomed.iter().any(|&i| lay.sealed[i]) {
        return Err(Error::Sealed);
    }
    let anchor_index = (pos > 0).then(|| lay.visible_index[pos - 1]);
    if !s.is_empty()
        && let Some(a) = anchor_index
        && lay
            .seal_spans
            .iter()
            .any(|&(first, last)| first <= a && a < last)
    {
        return Err(Error::Sealed);
    }
    let mut deletes = ranges_in_order(t, doomed.iter().map(|&i| lay.order[i]));
    // Characters hidden only by TL-45 (b) that duplicate a deleted one.
    // Without copies visible == vis_a, so nothing is hidden only by (b).
    if !doomed.is_empty() && t.has_copies() {
        let canon: std::collections::HashSet<CharId> =
            doomed.iter().map(|&i| t.canonical(lay.order[i])).collect();
        let hidden = (0..lay.order.len())
            .filter(|&i| lay.vis_a[i] && !lay.visible[i])
            .map(|i| lay.order[i])
            .filter(|&l| canon.contains(&t.canonical(l)));
        deletes.extend(ranges_in_order(t, hidden));
    }
    let mut inserts = Vec::new();
    if !s.is_empty() {
        inserts.push(Insert {
            anchor: anchor_index.map(|i| t.char_id(lay.order[i])),
            text: s,
        });
    }
    if deletes.is_empty() && inserts.is_empty() {
        return Ok(None);
    }
    Ok(Some(Op::TextEdit {
        target: target.clone(),
        deletes,
        inserts,
    }))
}

impl<S: Store> History<S> {
    /// Plans the local edit "at visible position `pos`, delete `delete` characters, insert
    /// `text`" of a text as one `text.edit` (TL-47); `None` when it changes nothing. Errors:
    /// `out_of_range`, `sealed` (TL-53), `unknown_object`.
    pub fn plan_edit(
        &self,
        at: &At,
        object: &ObjectId,
        pos: usize,
        delete: usize,
        text: &str,
    ) -> Result<Option<Op>, Error> {
        let state = self.state(at)?;
        let t = state.text_obj(object).ok_or(Error::UnknownObject)?;
        plan_edit(t, object, pos, delete, text)
    }

    /// Plans sealing visible positions `[from, to)` of a transcript (TL-51).
    pub fn plan_seal(
        &self,
        at: &At,
        object: &ObjectId,
        from: usize,
        to: usize,
    ) -> Result<Op, Error> {
        let state = self.state(at)?;
        let t = state.text_obj(object).ok_or(Error::UnknownObject)?;
        if !t.transcript {
            return Err(Error::UnknownObject);
        }
        let lay = t.layout();
        if from >= to || to > lay.visible_index.len() {
            return Err(Error::OutOfRange);
        }
        let ranges = ranges_in_order(t, lay.visible_index[from..to].iter().map(|&i| lay.order[i]));
        Ok(Op::TextSeal {
            target: object.clone(),
            ranges,
        })
    }
}
