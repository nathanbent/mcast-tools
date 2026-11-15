//! Plain text, as mcast-tools shows, sends and speaks it.
//!
//! Text comes from people (a message typed into `mcast text send`, a text
//! page in the intercom) and from the network (other senders' real-time
//! text, the names in RTCP and SAP). Either way it ends up on a terminal,
//! a web page, a sign or in a speech engine, so it goes through one of:
//!
//! - [`shown`]: text we send or put on a display. Emoji shortcodes
//!   (`:fire:`) become emoji, and what isn't safe to show is taken out.
//! - [`spoken`]: the same text for a speech engine, without the emoji
//!   (engines read them out as nonsense, or not at all).
//! - [`clean`]: text from somewhere else, made safe to show and otherwise
//!   left as it came.
//! - [`label`]: a name from somewhere else, on one line.

mod clean;
pub mod emoji;

pub use clean::{clean, label, MAX_MARKS};

/// Text to send or show: shortcodes become emoji, then [`clean`].
pub fn shown(text: &str) -> String {
    clean(&emoji::expand(text))
}

/// Text to speak: [`shown`] without its emoji, trimmed. Empty when there
/// was nothing but emoji.
pub fn spoken(text: &str) -> String {
    emoji::strip(&shown(text)).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_text_shown_and_spoken() {
        let page = ":warning: Fire drill at 3pm \u{1F525}\u{1b}[31m!";
        assert_eq!(shown(page), "\u{26A0}\u{FE0F} Fire drill at 3pm \u{1F525}!");
        assert_eq!(spoken(page), "Fire drill at 3pm!");
        assert_eq!(spoken(":fire::fire: \u{1F6A8}"), "");
        // Idempotent: what's been through once comes out the same.
        assert_eq!(shown(&shown(page)), shown(page));
        assert_eq!(spoken(&spoken(page)), spoken(page));
    }
}
