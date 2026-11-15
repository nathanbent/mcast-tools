//! Who said what: turns everything a receiver hears into labelled lines.
//!
//! The CLI (`mcast text recv`, `chat`) and the API's text channels both
//! show a channel this way, so the rules are here once: a sender goes by
//! its RTCP name once that's heard and its address until then, a
//! half-typed line is shown once its sender has been quiet for
//! [`STALE_LINE`], and what's left is shown when a sender leaves.
//!
//! Lines a sender marks as a page ([`crate::page`]) come out together, as
//! one [`Entry::Page`] with the page's name and priority, when the page
//! is complete.

use crate::lines::{Item, LineBuffer};
use crate::page::{Element, PageInfo};
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
    /// A whole page: lines that go up together, with what they are.
    Page {
        ssrc: u32,
        from: String,
        info: PageInfo,
        lines: Vec<String>,
    },
    /// A page comes down: after its hold, or at once with `now`.
    Clear {
        ssrc: u32,
        id: String,
        now: bool,
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
    /// The page each sender has begun and not yet finished.
    open: HashMap<u32, (PageInfo, Vec<String>)>,
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

    /// A line or an element from `ssrc`: lines of a page that's begun wait
    /// for it to be complete.
    fn item(&mut self, ssrc: u32, item: Item, out: &mut Vec<Entry>) {
        match item {
            Item::Line(text) => match self.open.get_mut(&ssrc) {
                Some((_, lines)) => {
                    if !text.trim().is_empty() {
                        lines.push(text);
                    }
                }
                None => out.push(self.line(ssrc, text)),
            },
            Item::Element(Element::Page(info)) => {
                // One never finished (its "show" was lost) goes out as it is.
                out.extend(self.close_page(ssrc));
                self.open.insert(ssrc, (info, Vec::new()));
            }
            Item::Element(Element::Show { id }) => {
                if self.open.get(&ssrc).is_some_and(|(info, _)| info.id == id) {
                    out.extend(self.close_page(ssrc));
                }
            }
            Item::Element(Element::Clear { id, now }) => out.push(Entry::Clear { ssrc, id, now }),
        }
    }

    /// The page `ssrc` has begun, as far as it's got.
    fn close_page(&mut self, ssrc: u32) -> Option<Entry> {
        let (info, lines) = self.open.remove(&ssrc)?;
        if lines.is_empty() {
            return None;
        }
        self.shown.insert(ssrc);
        Some(Entry::Page {
            ssrc,
            from: self.label(ssrc),
            info,
            lines,
        })
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
                let items = buf.push(text, now);
                let mut out = Vec::new();
                for item in items {
                    self.item(ssrc, item, &mut out);
                }
                out
            }
            Event::Bye { ssrc, reason } => {
                let mut out = Vec::new();
                if let Some(l) = self.buffers.remove(&ssrc).and_then(|mut b| b.take()) {
                    self.item(ssrc, Item::Line(l), &mut out);
                }
                out.extend(self.close_page(ssrc));
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
                let mut out = Vec::new();
                for (ssrc, text) in stale {
                    // A page gone quiet has lost its "show": it goes up now.
                    self.item(ssrc, Item::Line(text), &mut out);
                }
                let quiet: Vec<u32> = self
                    .open
                    .keys()
                    .copied()
                    .filter(|s| self.buffers.get(s).is_none_or(|b| b.quiet(now, STALE_LINE)))
                    .collect();
                for ssrc in quiet {
                    out.extend(self.close_page(ssrc));
                }
                out
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
        let mut out = Vec::new();
        for (ssrc, text) in left {
            self.item(ssrc, Item::Line(text), &mut out);
        }
        let open: Vec<u32> = self.open.keys().copied().collect();
        for ssrc in open {
            out.extend(self.close_page(ssrc));
        }
        out
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

    #[test]
    fn pages_come_out_whole() {
        use crate::page::{encode_page, Priority};
        let now = Instant::now();
        let mut t = Transcript::new();
        let info = PageInfo {
            id: "41.5".into(),
            name: "Fire drill".into(),
            priority: Priority::Emergency,
            hold_s: 10,
        };
        let text = |t: &mut Transcript, s: &str, at: Instant| {
            t.event(Event::Text { ssrc: 1, text: s }, at)
        };
        let page = |lines: &[&str]| Entry::Page {
            ssrc: 1,
            from: "00000001".into(),
            info: info.clone(),
            lines: lines.iter().map(|l| l.to_string()).collect(),
        };
        // Nothing until it's complete, then all of it; plain text around it is as ever.
        let sent = encode_page(&info, "Fire drill at 3pm\nUse the east stairs");
        let (first, rest) = sent.split_at(sent.find("east").unwrap());
        assert_eq!(
            text(&mut t, &format!("before\n{first}"), now),
            [line(1, "00000001", "before")]
        );
        assert_eq!(
            text(&mut t, &format!("{rest}after\n"), now),
            [
                page(&["Fire drill at 3pm", "Use the east stairs"]),
                line(1, "00000001", "after")
            ]
        );
        assert_eq!(
            text(&mut t, "\u{98}mc1 clear 41.5 now\u{9c}", now),
            [Entry::Clear {
                ssrc: 1,
                id: "41.5".into(),
                now: true
            }]
        );
        // "show" lost: the page goes up once its sender has gone quiet,
        // with the half line it had.
        let lost = sent.split(crate::page::SOS).nth(1).unwrap();
        assert_eq!(text(&mut t, &format!("\u{98}{lost}"), now), []);
        assert_eq!(t.event(Event::Tick, now + Duration::from_secs(1)), []);
        assert_eq!(
            t.event(Event::Tick, now + STALE_LINE),
            [page(&["Fire drill at 3pm", "Use the east stairs"])]
        );
        // Another sender's "show" doesn't finish it, and an empty page is nothing.
        text(&mut t, "\u{98}mc1 page 41.6 n 10 Next\u{9c}half", now);
        assert_eq!(
            t.event(
                Event::Text {
                    ssrc: 2,
                    text: "\u{98}mc1 show 41.6\u{9c}"
                },
                now
            ),
            []
        );
        assert_eq!(
            text(
                &mut t,
                "\u{98}mc1 page 41.7 n 10 Empty\u{9c}\u{98}mc1 show 41.7\u{9c}",
                now
            )
            .len(),
            1
        );
    }
}
