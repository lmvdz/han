//! A one-line text field with the editing keys a terminal user expects.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A single line being edited.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineEdit {
    text: String,
    /// The cursor, in characters.
    cursor: usize,
}

impl LineEdit {
    /// A field holding `text`, cursor at the end.
    pub fn with(text: &str) -> Self {
        LineEdit {
            text: text.to_owned(),
            cursor: text.chars().count(),
        }
    }

    /// The text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The cursor, in characters.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether nothing is typed.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn byte(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(i, _)| i)
    }

    /// Inserts text at the cursor (pasted text loses its line breaks and control characters).
    pub fn insert(&mut self, s: &str) {
        let s: String = s.chars().filter(|c| !c.is_control()).collect();
        let at = self.byte(self.cursor);
        self.text.insert_str(at, &s);
        self.cursor += s.chars().count();
    }

    /// Applies an editing key; false when the key is not an editing key.
    pub fn key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('u') if ctrl => {
                let at = self.byte(self.cursor);
                self.text.replace_range(..at, "");
                self.cursor = 0;
            }
            KeyCode::Char('w') if ctrl => {
                let before: Vec<char> = self.text.chars().take(self.cursor).collect();
                let mut start = before.len();
                while start > 0 && before[start - 1] == ' ' {
                    start -= 1;
                }
                while start > 0 && before[start - 1] != ' ' {
                    start -= 1;
                }
                let (a, b) = (self.byte(start), self.byte(self.cursor));
                self.text.replace_range(a..b, "");
                self.cursor = start;
            }
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.text.chars().count(),
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => {
                self.insert(&c.to_string());
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let (a, b) = (self.byte(self.cursor - 1), self.byte(self.cursor));
                self.text.replace_range(a..b, "");
                self.cursor -= 1;
            }
            KeyCode::Delete if self.cursor < self.text.chars().count() => {
                let (a, b) = (self.byte(self.cursor), self.byte(self.cursor + 1));
                self.text.replace_range(a..b, "");
            }
            KeyCode::Backspace | KeyCode::Delete => {}
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.text.chars().count()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.chars().count(),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn editing() {
        let mut e = LineEdit::default();
        for c in "héllo wörld".chars() {
            e.key(k(KeyCode::Char(c)));
        }
        assert_eq!(e.text(), "héllo wörld");
        e.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(e.text(), "héllo ");
        e.key(k(KeyCode::Left));
        e.key(k(KeyCode::Backspace));
        assert_eq!(e.text(), "héll ");
        e.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(e.text(), " ");
        assert!(!e.key(k(KeyCode::Esc)));
    }
}
