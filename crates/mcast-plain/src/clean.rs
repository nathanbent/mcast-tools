//! Making text safe to show.
//!
//! Nothing here escapes text for one particular place (HTML, a shell):
//! the front ends do that where they put text (`textContent`, never
//! markup; arguments and stdin, never a command line). This takes out
//! what no place should be shown: control characters and terminal escape
//! sequences (a received line could otherwise retitle a terminal or
//! rewrite what's above it), characters that reorder or hide the text
//! around them, and piles of combining marks that spill over other lines.

use crate::emoji::{is_tag, BLACK_FLAG, CANCEL_TAG};
use std::iter::Peekable;
use std::str::Chars;

/// Most combining marks kept on one character; more are dropped.
pub const MAX_MARKS: usize = 4;

/// Text made safe to show. Line breaks of every kind become `\n`; tabs
/// stay. Emoji are untouched, joiners and variation selectors included.
pub fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut marks = 0;
    // Just after a black flag (and the tags that followed it): tag
    // characters there spell a flag. Anywhere else they're invisible text.
    let mut in_flag = false;
    while let Some(c) = chars.next() {
        let was_flag = std::mem::take(&mut in_flag);
        if is_mark(c) {
            marks += 1;
            if marks <= MAX_MARKS {
                out.push(c);
            }
            continue;
        }
        marks = 0;
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' | '\t' => out.push(c),
            // Vertical tab, form feed, NEL, LINE and PARAGRAPH SEPARATOR.
            '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}' => out.push('\n'),
            '\u{1b}' => skip_escape(&mut chars),
            // The one-character form of `ESC [`.
            '\u{9b}' => skip_csi(&mut chars),
            c if is_tag(c) => {
                if was_flag {
                    out.push(c);
                    in_flag = c != CANCEL_TAG;
                }
            }
            c if hidden(c) => {}
            c => {
                in_flag = c == BLACK_FLAG;
                out.push(c);
            }
        }
    }
    out
}

/// A name from somewhere else (RTCP, SAP, a file's name), on one line:
/// [`clean`], with each run of white space a single space.
pub fn label(text: &str) -> String {
    clean(text).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Characters that are never shown: controls, those that reorder the text
/// around them (the "Trojan source" overrides and isolates), byte order
/// marks, annotation and deprecated format characters, noncharacters.
fn hidden(c: char) -> bool {
    let u = c as u32;
    c.is_control()
        || matches!(
            u,
            0x202A..=0x202E | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB | 0xFDD0..=0xFDEF
        )
        || u & 0xFFFE == 0xFFFE
}

/// Combining marks (the blocks that stack above and below a character).
fn is_mark(c: char) -> bool {
    matches!(
        c as u32,
        0x0300..=0x036F | 0x0483..=0x0489 | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20E2
            | 0x20E4..=0x20F0 | 0xFE20..=0xFE2F
    )
}

/// Skip the rest of an escape sequence, `ESC` already taken.
fn skip_escape(chars: &mut Peekable<Chars>) {
    match chars.peek() {
        Some('[') => {
            chars.next();
            skip_csi(chars);
        }
        // Strings (a window title, say): up to BEL or the string terminator,
        // and never past the end of the line.
        Some(']' | 'P' | 'X' | '^' | '_') => {
            chars.next();
            while let Some(&c) = chars.peek() {
                match c {
                    '\n' | '\r' => break,
                    '\u{7}' | '\u{9c}' => {
                        chars.next();
                        break;
                    }
                    '\u{1b}' => {
                        chars.next();
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                    _ => {
                        chars.next();
                    }
                }
            }
        }
        // Two- and three-character sequences: intermediates, then a final.
        _ => {
            while chars.next_if(|c| matches!(c, ' '..='/')).is_some() {}
            chars.next_if(|c| matches!(c, '0'..='~'));
        }
    }
}

/// Skip a control sequence's parameters, intermediates and final character.
fn skip_csi(chars: &mut Peekable<Chars>) {
    while chars.next_if(|c| matches!(c, '0'..='?')).is_some() {}
    while chars.next_if(|c| matches!(c, ' '..='/')).is_some() {}
    chars.next_if(|c| matches!(c, '@'..='~'));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_out_controls_and_escape_sequences() {
        assert_eq!(clean("a\u{7}b\u{0}c\u{7f}d\u{8}"), "abcd");
        assert_eq!(clean("\u{1b}[1;31mred\u{1b}[0m"), "red");
        assert_eq!(clean("\u{9b}2Jgone"), "gone");
        // A title, ended by BEL or ST; unterminated, it stops at the line's end.
        assert_eq!(clean("\u{1b}]0;pwned\u{7}ok"), "ok");
        assert_eq!(clean("\u{1b}]0;pwned\u{1b}\\ok"), "ok");
        assert_eq!(clean("\u{1b}]0;pwned\nnext"), "\nnext");
        assert_eq!(clean("\u{1b}(Bx \u{1b}cy \u{1b}"), "x y ");
        // An escape cut short doesn't eat the text after it.
        assert_eq!(clean("\u{1b}[31"), "");
        assert_eq!(clean("\u{1b}[\u{e9}t\u{e9}"), "\u{e9}t\u{e9}");
    }

    #[test]
    fn line_breaks_become_newlines_and_tabs_stay() {
        assert_eq!(
            clean("a\r\nb\rc\u{2028}d\u{2029}e\u{85}f\tg"),
            "a\nb\nc\nd\ne\nf\tg"
        );
    }

    #[test]
    fn takes_out_what_reorders_or_hides_text() {
        // "Trojan source": an override that shows the text backwards.
        assert_eq!(clean("pay \u{202E}001$\u{202C} now"), "pay 001$ now");
        assert_eq!(clean("\u{2066}a\u{2069}\u{FEFF}b\u{FFFE}"), "ab");
        // Marks that only say which way their own character runs stay.
        assert_eq!(clean("\u{200F}\u{5d0}"), "\u{200F}\u{5d0}");
        // Invisible tag characters, except where they spell a flag.
        assert_eq!(clean("hi\u{E0068}\u{E0069}"), "hi");
        let scotland = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}";
        assert_eq!(clean(scotland), scotland);
        assert_eq!(clean(&format!("{scotland}\u{E0041}")), scotland);
    }

    #[test]
    fn keeps_emoji_whole() {
        for e in [
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}", // a family, joined
            "\u{26A0}\u{FE0F}",                            // warning, as an emoji
            "\u{1F44D}\u{1F3FD}",                          // thumbs up, with a skin tone
            "\u{1F1EC}\u{1F1E7}",                          // a flag
            "1\u{FE0F}\u{20E3}",                           // keycap 1
        ] {
            assert_eq!(clean(e), e);
        }
        // And other scripts' joiners and accents.
        assert_eq!(
            clean("\u{645}\u{6cc}\u{200C}\u{62e}"),
            "\u{645}\u{6cc}\u{200C}\u{62e}"
        );
        assert_eq!(clean("cafe\u{301}"), "cafe\u{301}");
    }

    #[test]
    fn limits_stacked_marks() {
        let zalgo = format!("a{}b", "\u{301}".repeat(40));
        assert_eq!(clean(&zalgo), format!("a{}b", "\u{301}".repeat(MAX_MARKS)));
    }

    #[test]
    fn labels_are_one_line() {
        assert_eq!(label("  Front\n desk\t\u{1b}[5m 2 "), "Front desk 2");
        assert_eq!(label("\u{7}"), "");
    }
}
