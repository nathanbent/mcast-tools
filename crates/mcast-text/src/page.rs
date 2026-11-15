//! Pages: text that goes up on a display as a whole, with what a display
//! needs to show it properly. See `docs/text-page-metadata.md`.
//!
//! A text stream is only words. A page's name, priority and end ride in
//! the stream as T.140 control strings (SOS … ST), which a receiver that
//! doesn't know them leaves out of what it shows:
//!
//! ```text
//! ␘mc1 page ID PRIORITY HOLD name…␜first line
//! last line␘mc1 show ID␜
//! ```
//!
//! and, when the page is to come down, `␘mc1 clear ID␜` (or `… now`).

use std::fmt;

/// START OF STRING: begins an element.
pub const SOS: char = '\u{98}';
/// STRING TERMINATOR: ends it.
pub const ST: char = '\u{9c}';
/// T.140 allows a string this long, in bytes.
pub const MAX_ELEMENT: usize = 256;
/// The tag of this format, version 1.
const TAG: &str = "mc1";
/// Longest page id, in bytes.
pub const MAX_ID: usize = 64;

/// How urgent a page is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    #[default]
    Normal,
    High,
    Emergency,
}

impl Priority {
    pub fn name(self) -> &'static str {
        match self {
            Priority::Normal => "normal",
            Priority::High => "high",
            Priority::Emergency => "emergency",
        }
    }

    /// From its name or its first letter.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "n" | "normal" => Some(Priority::Normal),
            "h" | "high" => Some(Priority::High),
            "e" | "emergency" => Some(Priority::Emergency),
            _ => None,
        }
    }
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What a page is, apart from its lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageInfo {
    /// Tells this page from every other: letters, digits and `.-_`. The
    /// intercom panel uses `RUN.AT_MS`, as its API does.
    pub id: String,
    /// What to head it with: the announcement's name.
    pub name: String,
    pub priority: Priority,
    /// Seconds it stays up after it's cleared.
    pub hold_s: u32,
}

/// Whether `id` can be a page's id.
pub fn is_page_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c))
}

/// One control string of a text stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Element {
    /// A page begins; its lines follow as ordinary text.
    Page(PageInfo),
    /// The page is complete: show it.
    Show { id: String },
    /// The page comes down: after its hold, or at once with `now`.
    Clear { id: String, now: bool },
}

impl Element {
    /// As sent: SOS, the element, ST. A page's name is made one line and
    /// cut to fit T.140's limit.
    pub fn encode(&self) -> String {
        let body = match self {
            Element::Page(p) => {
                let head = format!(
                    "{TAG} page {} {} {} ",
                    p.id,
                    &p.priority.name()[..1],
                    p.hold_s
                );
                let name = mcast_plain::label(&p.name);
                let (name, _) =
                    crate::t140::split_at_most(&name, MAX_ELEMENT.saturating_sub(head.len()));
                format!("{head}{name}")
            }
            Element::Show { id } => format!("{TAG} show {id}"),
            Element::Clear { id, now: false } => format!("{TAG} clear {id}"),
            Element::Clear { id, now: true } => format!("{TAG} clear {id} now"),
        };
        format!("{SOS}{body}{ST}")
    }

    /// From what was between SOS and ST; `None` for anything else's
    /// strings, or one of ours that makes no sense.
    pub fn parse(body: &str) -> Option<Self> {
        let rest = body.strip_prefix(TAG)?.strip_prefix(' ')?;
        let (kind, rest) = rest.split_once(' ')?;
        let id = |s: &str| is_page_id(s).then(|| s.to_string());
        match kind {
            "page" => {
                let mut parts = rest.splitn(4, ' ');
                let id = id(parts.next()?)?;
                let priority = Priority::parse(parts.next()?)?;
                let hold_s = parts.next()?.parse().ok()?;
                let name = mcast_plain::label(parts.next().unwrap_or(""));
                Some(Element::Page(PageInfo {
                    id,
                    name,
                    priority,
                    hold_s,
                }))
            }
            "show" => Some(Element::Show { id: id(rest)? }),
            "clear" => match rest.split_once(' ') {
                None => Some(Element::Clear {
                    id: id(rest)?,
                    now: false,
                }),
                Some((i, "now")) => Some(Element::Clear {
                    id: id(i)?,
                    now: true,
                }),
                Some(_) => None,
            },
            _ => None,
        }
    }
}

/// A page as sent: its beginning, its lines, and "show" before the last
/// line's end, so a receiver that only drops the elements shows the lines
/// and no blank one. `text` is plain text; `\n` ends a line.
pub fn encode_page(info: &PageInfo, text: &str) -> String {
    let show = Element::Show {
        id: info.id.clone(),
    };
    format!(
        "{}{}{}\n",
        Element::Page(info.clone()).encode(),
        text.trim_end_matches('\n'),
        show.encode()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> PageInfo {
        PageInfo {
            id: "41.1794764572215".into(),
            name: "Fire drill".into(),
            priority: Priority::Emergency,
            hold_s: 10,
        }
    }

    #[test]
    fn elements_roundtrip() {
        let body = |e: &Element| {
            let s = e.encode();
            assert!(s.starts_with(SOS) && s.ends_with(ST), "{s:?}");
            assert!(s.len() - 4 <= MAX_ELEMENT);
            s[2..s.len() - 2].to_string()
        };
        let page = Element::Page(info());
        assert_eq!(body(&page), "mc1 page 41.1794764572215 e 10 Fire drill");
        for e in [
            page,
            Element::Show { id: "a-1".into() },
            Element::Clear {
                id: "a-1".into(),
                now: false,
            },
            Element::Clear {
                id: "a-1".into(),
                now: true,
            },
        ] {
            assert_eq!(Element::parse(&body(&e)), Some(e));
        }
    }

    #[test]
    fn names_are_one_line_and_cut_to_fit() {
        let e = Element::Page(PageInfo {
            name: format!("Lunch\n\u{1F355}\u{9c} {}", "\u{e9}".repeat(300)),
            ..info()
        });
        let s = e.encode();
        // One terminator, at the end: the name can't end the string early.
        assert_eq!(s.matches(ST).count(), 1);
        assert_eq!(s.len() - 4, MAX_ELEMENT);
        let Some(Element::Page(p)) = Element::parse(&s[2..s.len() - 2]) else {
            panic!("{s:?}")
        };
        assert!(p.name.starts_with("Lunch \u{1F355} \u{e9}"), "{}", p.name);
    }

    #[test]
    fn ignores_what_it_does_not_know() {
        for body in [
            "",
            "mc2 show a",
            "mc1 dance a",
            "mc1 show",
            "mc1 show a b",
            "mc1 show ../x",
            "mc1 clear a later",
            "mc1 page a x 10 Name",
            "mc1 page a e ten Name",
            "mc1 page a e",
            "someone else's string",
        ] {
            assert_eq!(Element::parse(body), None, "{body:?}");
        }
        // A page may have no name.
        assert!(matches!(
            Element::parse("mc1 page a n 5"),
            Some(Element::Page(p)) if p.name.is_empty() && p.hold_s == 5
        ));
    }

    #[test]
    fn a_page_shows_before_its_last_line_ends() {
        let sent = encode_page(&info(), "one\ntwo\n");
        assert!(
            sent.ends_with("two\u{98}mc1 show 41.1794764572215\u{9c}\n"),
            "{sent:?}"
        );
        // What a receiver that only drops the strings shows.
        assert_eq!(mcast_plain::clean(&sent), "one\ntwo\n");
    }
}
