//! Text helpers: sanitising what other people and agents wrote (SR-97), fitting to columns, and the compact
//! forms of numbers and ages.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Makes text from elsewhere safe to put on a terminal: control characters (an ESC would start an escape
/// sequence) and bidirectional overrides become visible `<U+XXXX>` escapes, and line breaks become spaces.
pub fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' | '\r' | '\t' => out.push(' '),
            c if c.is_control() || is_bidi(c) => out.push_str(&format!("<U+{:04X}>", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn is_bidi(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{061C}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// The text cut to at most `width` columns, with an ellipsis when it was cut.
pub fn fit(s: &str, width: usize) -> String {
    if UnicodeWidthStr::width(s) <= width {
        return s.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// The text fitted, then padded with spaces to exactly `width` columns.
pub fn pad(s: &str, width: usize) -> String {
    let mut out = fit(s, width);
    let used = UnicodeWidthStr::width(out.as_str());
    out.extend(std::iter::repeat_n(' ', width.saturating_sub(used)));
    out
}

/// Word-wraps text to `width` columns (long words are split).
pub fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for para in s.split('\n') {
        let mut line = String::new();
        let mut used = 0;
        for word in para.split(' ') {
            let w = UnicodeWidthStr::width(word);
            if w > width {
                if !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                    used = 0;
                }
                for c in word.chars() {
                    let cw = UnicodeWidthChar::width(c).unwrap_or(0);
                    if used + cw > width {
                        lines.push(std::mem::take(&mut line));
                        used = 0;
                    }
                    line.push(c);
                    used += cw;
                }
                continue;
            }
            let sep = usize::from(!line.is_empty());
            if used + sep + w > width {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            if !line.is_empty() {
                line.push(' ');
                used += 1;
            }
            line.push_str(word);
            used += w;
        }
        lines.push(line);
    }
    lines
}

/// 41212 as "41k", 1_200_000 as "1.2M".
pub fn count(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=9_999 => format!("{:.1}k", n as f64 / 1000.0),
        10_000..=999_999 => format!("{}k", n / 1000),
        1_000_000..=9_999_999 => format!("{:.1}M", n as f64 / 1_000_000.0),
        _ => format!("{}M", n / 1_000_000),
    }
}

/// An age in milliseconds as "20s", "4m", "2h", "3d".
pub fn age(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

/// A digest in groups of four characters, for reading it aloud and comparing it by eye.
pub fn digest_groups(d: &str) -> String {
    let d = d.strip_prefix("sha256:").unwrap_or(d);
    d.chars()
        .collect::<Vec<_>>()
        .chunks(4)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_text_is_made_visible() {
        assert_eq!(clean("a\u{1b}[31mb"), "a<U+001B>[31mb");
        assert_eq!(clean("x\u{202E}y"), "x<U+202E>y");
        assert_eq!(clean("<b>x</b>\nz"), "<b>x</b> z");
    }

    #[test]
    fn fitting_and_compacting() {
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("abc", 4), "abc");
        assert_eq!(pad("ab", 4), "ab  ");
        assert_eq!(count(41_212), "41k");
        assert_eq!(count(1_200_000), "1.2M");
        assert_eq!(age(20_000), "20s");
        assert_eq!(age(4 * 60_000), "4m");
        assert_eq!(digest_groups("sha256:abcdefgh12"), "abcd efgh 12");
        assert_eq!(wrap("aa bb cc", 5), vec!["aa bb", "cc"]);
    }
}
