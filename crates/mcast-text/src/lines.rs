//! Turns a source's stream of real-time text into lines for display.

use crate::t140::BACKSPACE;
use std::time::{Duration, Instant};

/// Longest line, in bytes: a sender that never ends its line has it ended
/// for it here.
pub const MAX_LINE: usize = 8000;

/// Collects one source's text until a line ends. Real-time text can arrive
/// a character at a time (and be erased with backspace), so the partial line
/// is kept here and shown once it ends or the sender stops typing.
///
/// Lines come out safe to show ([`mcast_plain::clean`]): the text is
/// whatever someone on the network sent.
#[derive(Debug, Clone)]
pub struct LineBuffer {
    partial: String,
    last: Instant,
}

impl LineBuffer {
    pub fn new(now: Instant) -> Self {
        Self {
            partial: String::new(),
            last: now,
        }
    }

    /// Add received text (already converted with `from_t140`); returns the
    /// lines it completed.
    pub fn push(&mut self, text: &str, now: Instant) -> Vec<String> {
        self.last = now;
        let mut done = Vec::new();
        for c in text.chars() {
            match c {
                '\n' => done.push(self.finish()),
                // Erasing past the start of the line can't undo what's shown.
                BACKSPACE => {
                    self.partial.pop();
                }
                c => {
                    self.partial.push(c);
                    if self.partial.len() >= MAX_LINE {
                        done.push(self.finish());
                    }
                }
            }
        }
        done
    }

    fn finish(&mut self) -> String {
        mcast_plain::clean(&std::mem::take(&mut self.partial))
    }

    /// The unfinished line, if the sender has gone quiet for `after`.
    pub fn take_stale(&mut self, now: Instant, after: Duration) -> Option<String> {
        // Nothing left once it's made safe to show (only control strings,
        // say) is nothing to show.
        (!self.partial.is_empty() && now.duration_since(self.last) >= after)
            .then(|| self.finish())
            .filter(|l| !l.is_empty())
    }

    /// The unfinished line, whatever its age.
    pub fn take(&mut self) -> Option<String> {
        (!self.partial.is_empty())
            .then(|| self.finish())
            .filter(|l| !l.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_backspace_and_staleness() {
        let t0 = Instant::now();
        let mut b = LineBuffer::new(t0);
        assert!(b.push("hel", t0).is_empty());
        assert_eq!(b.push("p\u{8}lo\nwor", t0), vec!["hello".to_string()]);
        assert_eq!(
            b.take_stale(t0 + Duration::from_secs(1), Duration::from_secs(3)),
            None
        );
        assert_eq!(
            b.take_stale(t0 + Duration::from_secs(3), Duration::from_secs(3)),
            Some("wor".into())
        );
        assert_eq!(b.push("\n\n", t0), vec![String::new(), String::new()]);
        assert_eq!(b.take(), None);
    }

    #[test]
    fn lines_come_out_safe_to_show_with_their_emoji() {
        let t0 = Instant::now();
        let mut b = LineBuffer::new(t0);
        // An escape sequence split across packets, and a joined emoji.
        assert!(b.push("\u{1b}]0;ti", t0).is_empty());
        assert_eq!(
            b.push("tle\u{7}Fire \u{1F468}\u{200D}\u{1F692}\u{1b}[2J\n", t0),
            vec!["Fire \u{1F468}\u{200D}\u{1F692}".to_string()]
        );
        // Backspace takes an emoji in one go when it's one character.
        assert_eq!(
            b.push("ok \u{1F525}\u{8}\u{2705}\n", t0),
            vec!["ok \u{2705}".to_string()]
        );
        // A half-typed line of nothing that's shown isn't a line.
        assert!(b.push("\u{98}for a program\u{9c}", t0).is_empty());
        assert_eq!(
            b.take_stale(t0 + Duration::from_secs(9), Duration::from_secs(3)),
            None
        );
        assert!(b.push("\u{98}again\u{9c}", t0).is_empty());
        assert_eq!(b.take(), None);
        // A line that never ends is ended for it.
        let long = b.push(&"x".repeat(MAX_LINE + 10), t0);
        assert_eq!(long.len(), 1);
        assert_eq!(long[0].len(), MAX_LINE);
        assert_eq!(b.take(), Some("x".repeat(10)));
    }
}
