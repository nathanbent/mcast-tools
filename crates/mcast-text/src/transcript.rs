//! Who said what: turns everything a receiver hears into labelled lines.
//!
//! The CLI (`mcast text recv`, `chat`) and the API's text channels both
//! show a channel this way, so the rules are here once: a sender goes by
//! its RTCP name once that's heard and its address until then, a
//! half-typed line is shown once its sender has been quiet for
//! [`STALE_LINE`], and what's left is shown when a sender leaves.

use crate::lines::LineBuffer;
use crate::receiver::Event;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

/// Show a half-typed line once its sender has been quiet this long.
pub const STALE_LINE: Duration = Duration::from_secs(3);

/// Something to show for a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A line of text, labelled with who sent it.
    Line {
        ssrc: u32,
        from: String,
        text: String,
    },
    /// Text from a sender not heard before.
    Joined {
        ssrc: u32,
        from: SocketAddr,
    },
    /// A sender's name, first learned or changed.
    Named {
        ssrc: u32,
        name: String,
        /// Nobody else here goes by it.
        new: bool,
        /// Lines of theirs have already gone out under another label.
        relabel: bool,
    },
    /// A sender said goodbye.
    Left {
        who: String,
        reason: Option<String>,
    },
    UnknownPayloadType(u8),
    Warning(String),
}

/// A channel's senders and their unfinished lines.
#[derive(Debug, Default)]
pub struct Transcript {
    labels: HashMap<u32, String>,
    /// Sources whose lines have gone out.
    shown: HashSet<u32>,
    buffers: HashMap<u32, LineBuffer>,
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    /// Senders heard so far.
    pub fn senders(&self) -> usize {
        self.labels.len()
    }

    /// What `ssrc` goes by: its name, else its address, else its SSRC.
    pub fn label(&self, ssrc: u32) -> String {
        self.labels
            .get(&ssrc)
            .cloned()
            .unwrap_or_else(|| format!("{ssrc:08x}"))
    }

    fn line(&mut self, ssrc: u32, text: String) -> Entry {
        self.shown.insert(ssrc);
        Entry::Line {
            ssrc,
            from: self.label(ssrc),
            text,
        }
    }

    /// What a receiver event adds to the channel.
    pub fn event(&mut self, e: Event<'_>, now: Instant) -> Vec<Entry> {
        match e {
            Event::NewSource { ssrc, from } => {
                self.labels
                    .entry(ssrc)
                    .or_insert_with(|| from.ip().to_string());
                vec![Entry::Joined { ssrc, from }]
            }
            Event::Named { ssrc, name } => {
                // RTCP comes on its own socket, so a name can arrive after
                // the first lines.
                if name.is_empty() || self.labels.get(&ssrc) == Some(&name) {
                    return Vec::new();
                }
                let new = !self.labels.values().any(|l| *l == name);
                self.labels.insert(ssrc, name.clone());
                vec![Entry::Named {
                    ssrc,
                    name,
                    new,
                    relabel: self.shown.contains(&ssrc),
                }]
            }
            Event::Text { ssrc, text } => {
                let buf = self
                    .buffers
                    .entry(ssrc)
                    .or_insert_with(|| LineBuffer::new(now));
                let lines = buf.push(text, now);
                lines.into_iter().map(|l| self.line(ssrc, l)).collect()
            }
            Event::Bye { ssrc, reason } => {
                let mut out = Vec::new();
                if let Some(l) = self.buffers.remove(&ssrc).and_then(|mut b| b.take()) {
                    out.push(self.line(ssrc, l));
                }
                out.push(Entry::Left {
                    who: self.label(ssrc),
                    reason,
                });
                out
            }
            Event::UnknownPayloadType { payload_type } => {
                vec![Entry::UnknownPayloadType(payload_type)]
            }
            Event::Warning { message } => vec![Entry::Warning(message)],
            Event::Tick => {
                let stale: Vec<(u32, String)> = self
                    .buffers
                    .iter_mut()
                    .filter_map(|(&ssrc, b)| b.take_stale(now, STALE_LINE).map(|t| (ssrc, t)))
                    .collect();
                stale.into_iter().map(|(s, t)| self.line(s, t)).collect()
            }
        }
    }

    /// The lines still unfinished, when receiving ends.
    pub fn finish(&mut self) -> Vec<Entry> {
        let left: Vec<(u32, String)> = self
            .buffers
            .iter_mut()
            .filter_map(|(&ssrc, b)| b.take().map(|t| (ssrc, t)))
            .collect();
        left.into_iter().map(|(s, t)| self.line(s, t)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(ssrc: u32, from: &str, text: &str) -> Entry {
        Entry::Line {
            ssrc,
            from: from.into(),
            text: text.into(),
        }
    }

    #[test]
    fn labels_lines_and_relabels_when_a_name_comes_late() {
        let now = Instant::now();
        let from: SocketAddr = "10.0.0.7:5004".parse().unwrap();
        let mut t = Transcript::new();
        assert_eq!(
            t.event(Event::NewSource { ssrc: 1, from }, now),
            [Entry::Joined { ssrc: 1, from }]
        );
        let text = |t: &mut Transcript, s: &str| t.event(Event::Text { ssrc: 1, text: s }, now);
        assert_eq!(text(&mut t, "hi\nthe"), [line(1, "10.0.0.7", "hi")]);
        let named = |t: &mut Transcript, ssrc, name: &str| {
            t.event(
                Event::Named {
                    ssrc,
                    name: name.into(),
                },
                now,
            )
        };
        assert_eq!(
            named(&mut t, 1, "Ops"),
            [Entry::Named {
                ssrc: 1,
                name: "Ops".into(),
                new: true,
                relabel: true
            }]
        );
        // The same name again is nothing new; a second sender with it isn't "new".
        assert_eq!(named(&mut t, 1, "Ops"), []);
        assert_eq!(
            named(&mut t, 2, "Ops"),
            [Entry::Named {
                ssrc: 2,
                name: "Ops".into(),
                new: false,
                relabel: false
            }]
        );
        assert_eq!(t.senders(), 2);
        // A half-typed line shows once its sender goes quiet.
        assert_eq!(t.event(Event::Tick, now + Duration::from_secs(1)), []);
        assert_eq!(
            t.event(Event::Tick, now + STALE_LINE),
            [line(1, "Ops", "the")]
        );
        // What's left shows when the sender leaves, or receiving ends.
        text(&mut t, "bye");
        assert_eq!(
            t.event(
                Event::Bye {
                    ssrc: 1,
                    reason: None
                },
                now
            ),
            [
                line(1, "Ops", "bye"),
                Entry::Left {
                    who: "Ops".into(),
                    reason: None
                }
            ]
        );
        t.event(
            Event::Text {
                ssrc: 9,
                text: "end",
            },
            now,
        );
        assert_eq!(t.finish(), [line(9, "00000009", "end")]);
        assert_eq!(t.finish(), []);
    }
}
