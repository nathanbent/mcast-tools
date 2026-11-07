//! Turns a source's stream of real-time text into lines for display.

use crate::t140::BACKSPACE;
use std::time::{Duration, Instant};

/// Collects one source's text until a line ends. Real-time text can arrive
/// a character at a time (and be erased with backspace), so the partial line
/// is kept here and shown once it ends or the sender stops typing.
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
                '\n' => done.push(std::mem::take(&mut self.partial)),
                // Erasing past the start of the line can't undo what's shown.
                BACKSPACE => {
                    self.partial.pop();
                }
                c => self.partial.push(c),
            }
        }
        done
    }

    /// The unfinished line, if the sender has gone quiet for `after`.
    pub fn take_stale(&mut self, now: Instant, after: Duration) -> Option<String> {
        (!self.partial.is_empty() && now.duration_since(self.last) >= after)
            .then(|| std::mem::take(&mut self.partial))
    }

    /// The unfinished line, whatever its age.
    pub fn take(&mut self) -> Option<String> {
        (!self.partial.is_empty()).then(|| std::mem::take(&mut self.partial))
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
}
