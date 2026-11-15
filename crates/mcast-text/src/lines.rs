//! Turns a source's stream of real-time text into lines for display.

use crate::page::{Element, MAX_ELEMENT, SOS, ST};
use crate::t140::BACKSPACE;
use std::time::{Duration, Instant};

/// Longest line, in bytes: a sender that never ends its line has it ended
/// for it here.
pub const MAX_LINE: usize = 8000;

/// What a source's text adds up to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A line, safe to show.
    Line(String),
    /// A page element that came in the text (see [`crate::page`]).
    Element(Element),
}

/// Collects one source's text until a line ends. Real-time text can arrive
/// a character at a time (and be erased with backspace), so the partial line
/// is kept here and shown once it ends or the sender stops typing.
///
/// Lines come out safe to show ([`mcast_plain::clean`]): the text is
/// whatever someone on the network sent. Control strings (SOS … ST) are
/// taken out as they come; those that are page elements are handed on.
#[derive(Debug, Clone)]
pub struct LineBuffer {
    partial: String,
    /// The control string being received, if one is.
    control: Option<String>,
    /// An element just ended the line: the line break after it is its own.
    ended_by_element: bool,
    last: Instant,
}

impl LineBuffer {
    pub fn new(now: Instant) -> Self {
        Self {
            partial: String::new(),
            control: None,
            ended_by_element: false,
            last: now,
        }
    }

    /// Add received text (already converted with `from_t140`); returns the
    /// lines it completed and the page elements in it, in order.
    pub fn push(&mut self, text: &str, now: Instant) -> Vec<Item> {
        self.last = now;
        let mut done = Vec::new();
        for c in text.chars() {
            if let Some(control) = &mut self.control {
                match c {
                    ST => {
                        let body = self.control.take().unwrap_or_default();
                        if let Some(e) = Element::parse(&body) {
                            // An element ends the line before it: "show"
                            // comes before the last line's own end.
                            if !self.partial.is_empty() {
                                done.push(Item::Line(self.finish()));
                                self.ended_by_element = true;
                            }
                            done.push(Item::Element(e));
                        }
                        continue;
                    }
                    // Never ended (its end was lost, or it's not one of
                    // ours): give up on it at the end of the line.
                    '\n' => self.control = None,
                    _ => {
                        if control.len() < MAX_ELEMENT {
                            control.push(c);
                        }
                        continue;
                    }
                }
            }
            let after_element = std::mem::take(&mut self.ended_by_element);
            match c {
                '\n' if after_element => {}
                '\n' => done.push(Item::Line(self.finish())),
                SOS => self.control = Some(String::new()),
                // Erasing past the start of the line can't undo what's shown.
                BACKSPACE => {
                    self.partial.pop();
                }
                c => {
                    self.partial.push(c);
                    if self.partial.len() >= MAX_LINE {
                        done.push(Item::Line(self.finish()));
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

    /// Whether nothing has come for `after`.
    pub fn quiet(&self, now: Instant, after: Duration) -> bool {
        now.duration_since(self.last) >= after
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
        let line = |s: &str| Item::Line(s.to_string());
        assert!(b.push("hel", t0).is_empty());
        assert_eq!(b.push("p\u{8}lo\nwor", t0), vec![line("hello")]);
        assert_eq!(
            b.take_stale(t0 + Duration::from_secs(1), Duration::from_secs(3)),
            None
        );
        assert_eq!(
            b.take_stale(t0 + Duration::from_secs(3), Duration::from_secs(3)),
            Some("wor".into())
        );
        assert_eq!(b.push("\n\n", t0), vec![line(""), line("")]);
        assert_eq!(b.take(), None);
    }

    #[test]
    fn lines_come_out_safe_to_show_with_their_emoji() {
        let t0 = Instant::now();
        let mut b = LineBuffer::new(t0);
        let line = |s: &str| Item::Line(s.to_string());
        // An escape sequence split across packets, and a joined emoji.
        assert!(b.push("\u{1b}]0;ti", t0).is_empty());
        assert_eq!(
            b.push("tle\u{7}Fire \u{1F468}\u{200D}\u{1F692}\u{1b}[2J\n", t0),
            vec![line("Fire \u{1F468}\u{200D}\u{1F692}")]
        );
        // Backspace takes an emoji in one go when it's one character.
        assert_eq!(
            b.push("ok \u{1F525}\u{8}\u{2705}\n", t0),
            vec![line("ok \u{2705}")]
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
        assert_eq!(long[0], line(&"x".repeat(MAX_LINE)));
        assert_eq!(b.take(), Some("x".repeat(10)));
    }

    #[test]
    fn hands_on_page_elements_in_order() {
        use crate::page::{encode_page, PageInfo, Priority};
        let t0 = Instant::now();
        let mut b = LineBuffer::new(t0);
        let line = |s: &str| Item::Line(s.to_string());
        let info = PageInfo {
            id: "7.1".into(),
            name: "Bus call".into(),
            priority: Priority::High,
            hold_s: 10,
        };
        let sent = encode_page(&info, "Buses are here\nWalk");
        // Whole, and a character at a time (as packets may split it).
        let whole = b.push(&sent, t0);
        let mut slow = Vec::new();
        for c in sent.chars() {
            slow.extend(b.push(&c.to_string(), t0));
        }
        let expected = vec![
            Item::Element(Element::Page(info)),
            line("Buses are here"),
            line("Walk"),
            Item::Element(Element::Show { id: "7.1".into() }),
        ];
        assert_eq!(whole, expected);
        assert_eq!(slow, expected);
        assert_eq!(b.take(), None);
        // A string that's lost its end stops at the end of the line; text
        // after an element that follows no text keeps its line break.
        assert_eq!(
            b.push("a\u{98}mc1 show 7.1\nb\n", t0),
            vec![line("a"), line("b")]
        );
        assert_eq!(
            b.push("\u{98}mc1 clear 7.1 now\u{9c}\nc\n", t0),
            vec![
                Item::Element(Element::Clear {
                    id: "7.1".into(),
                    now: true
                }),
                line(""),
                line("c")
            ]
        );
    }
}
