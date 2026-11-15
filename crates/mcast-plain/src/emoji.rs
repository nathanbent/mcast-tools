//! Emoji: shortcodes for typing them, and telling them from the words
//! around them (a speech engine gets only the words).

/// VARIATION SELECTOR-16: "show the character before as an emoji".
pub const VS16: char = '\u{FE0F}';
/// VARIATION SELECTOR-15: "show the character before as text".
pub const VS15: char = '\u{FE0E}';
/// ZERO WIDTH JOINER: joins emoji into one (a family, a profession).
pub const ZWJ: char = '\u{200D}';
/// COMBINING ENCLOSING KEYCAP: makes a digit, `#` or `*` a key.
pub const KEYCAP: char = '\u{20E3}';
/// The flag whose tag characters spell a region (England, Scotland, Wales).
pub const BLACK_FLAG: char = '\u{1F3F4}';
/// Ends a flag's tag characters.
pub const CANCEL_TAG: char = '\u{E007F}';

/// Tag characters: invisible copies of ASCII, used to spell a flag's region.
pub fn is_tag(c: char) -> bool {
    matches!(c as u32, 0xE0000..=0xE007F)
}

/// Whether `c` is a pictograph on its own: the emoji blocks, and the older
/// symbol blocks most emoji fonts draw (weather, dingbats, clocks, stars).
/// Characters that are emoji only with [`VS16`] after them (`©`, `↗`)
/// aren't; [`strip`] looks for the selector.
pub fn is_emoji(c: char) -> bool {
    matches!(
        c as u32,
        0x1F000..=0x1FAFF | 0x2300..=0x23FF | 0x2600..=0x27BF | 0x2B00..=0x2BFF
    )
}

fn is_skin_tone(c: char) -> bool {
    matches!(c as u32, 0x1F3FB..=0x1F3FF)
}

fn is_regional(c: char) -> bool {
    matches!(c as u32, 0x1F1E6..=0x1F1FF)
}

/// The emoji a shortcode stands for: `shortcode("fire")` is 🔥.
pub fn shortcode(name: &str) -> Option<&'static str> {
    SHORTCODES
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, e)| *e)
}

/// Longest shortcode name looked for.
const MAX_NAME: usize = 32;

/// Replace the shortcodes in `text` (`:fire:`) with their emoji. Only names
/// in [`SHORTCODES`] are replaced, so `10:30:00` and `a:b:c` stay as typed.
pub fn expand(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(':') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let name_len = after
            .bytes()
            .take(MAX_NAME + 1)
            .position(|b| !(b.is_ascii_alphanumeric() || b"_+-".contains(&b)));
        match name_len {
            Some(n) if after.as_bytes()[n] == b':' => match shortcode(&after[..n]) {
                Some(e) => {
                    out.push_str(e);
                    rest = &after[n + 1..];
                }
                // Not a name we know: its closing colon may open the next.
                None => {
                    out.push(':');
                    out.push_str(&after[..n]);
                    rest = &after[n..];
                }
            },
            _ => {
                out.push(':');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `text` without its emoji, for a speech engine. The words around an
/// emoji close up (`"Lunch 🍕!"` is `"Lunch!"`), and a keycap leaves its
/// digit (`"Gate 2️⃣"` is `"Gate 2"`).
pub fn strip(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    // Emoji were dropped since the last character kept.
    let mut gap = false;
    // The last character seen, joiners and selectors aside, was an emoji.
    let mut after_emoji = false;
    while let Some(c) = chars.next() {
        match c {
            VS15 | VS16 | KEYCAP => continue,
            ZWJ if after_emoji => continue,
            c if is_tag(c) => continue,
            _ => {}
        }
        let key = c.is_ascii_digit() || c == '#' || c == '*';
        after_emoji = is_emoji(c) || (chars.peek() == Some(&VS16) && !key);
        if after_emoji {
            gap = true;
            continue;
        }
        if std::mem::take(&mut gap) {
            let spaced = out.is_empty() || out.ends_with(char::is_whitespace);
            if ".,!?;:)\n".contains(c) {
                // "Lunch 🍕!": no space left before the punctuation, or
                // at the end of a line.
                let kept = out.trim_end_matches(' ').len();
                out.truncate(kept);
            } else if c == ' ' && spaced {
                continue;
            } else if !spaced && !c.is_whitespace() {
                out.push(' ');
            }
        }
        out.push(c);
    }
    out
}

/// How many characters a reader sees in `s`: an emoji made of several code
/// points (a flag, a family, one with a skin tone) counts once. Close
/// enough for limits on short labels; not full grapheme segmentation.
pub fn count(s: &str) -> usize {
    let mut n = 0;
    let mut joined = false;
    let mut flag_half = false;
    for c in s.chars() {
        let pair = std::mem::take(&mut flag_half);
        if matches!(c, VS15 | VS16 | KEYCAP) || is_tag(c) || is_skin_tone(c) {
            continue;
        }
        if c == ZWJ {
            joined = true;
            continue;
        }
        if std::mem::take(&mut joined) || (pair && is_regional(c)) {
            continue;
        }
        flag_half = is_regional(c);
        n += 1;
    }
    n
}

/// Shortcodes, by the names GitHub and Slack use: the ones a notice, a
/// page or a sign is likely to want. Any emoji can also be typed as itself.
pub const SHORTCODES: &[(&str, &str)] = &[
    // Urgent
    ("warning", "\u{26A0}\u{FE0F}"),
    ("rotating_light", "\u{1F6A8}"),
    ("siren", "\u{1F6A8}"),
    ("fire", "\u{1F525}"),
    ("fire_extinguisher", "\u{1F9EF}"),
    ("fire_engine", "\u{1F692}"),
    ("ambulance", "\u{1F691}"),
    ("police_car", "\u{1F693}"),
    ("sos", "\u{1F198}"),
    ("no_entry", "\u{26D4}"),
    ("no_entry_sign", "\u{1F6AB}"),
    ("stop_sign", "\u{1F6D1}"),
    ("octagonal_sign", "\u{1F6D1}"),
    ("exclamation", "\u{2757}"),
    ("bangbang", "\u{203C}\u{FE0F}"),
    ("question", "\u{2753}"),
    ("x", "\u{274C}"),
    ("radioactive", "\u{2622}\u{FE0F}"),
    ("biohazard", "\u{2623}\u{FE0F}"),
    ("skull_and_crossbones", "\u{2620}\u{FE0F}"),
    ("zap", "\u{26A1}"),
    ("boom", "\u{1F4A5}"),
    ("construction", "\u{1F6A7}"),
    ("lock", "\u{1F512}"),
    ("unlock", "\u{1F513}"),
    ("key", "\u{1F511}"),
    ("shield", "\u{1F6E1}\u{FE0F}"),
    // Notices
    ("bell", "\u{1F514}"),
    ("no_bell", "\u{1F515}"),
    ("loudspeaker", "\u{1F4E2}"),
    ("mega", "\u{1F4E3}"),
    ("speaker", "\u{1F508}"),
    ("loud_sound", "\u{1F50A}"),
    ("mute", "\u{1F507}"),
    ("microphone", "\u{1F3A4}"),
    ("speech_balloon", "\u{1F4AC}"),
    ("information_source", "\u{2139}\u{FE0F}"),
    ("info", "\u{2139}\u{FE0F}"),
    ("white_check_mark", "\u{2705}"),
    ("check", "\u{2705}"),
    ("heavy_check_mark", "\u{2714}\u{FE0F}"),
    ("ok", "\u{1F197}"),
    ("new", "\u{1F195}"),
    ("pushpin", "\u{1F4CC}"),
    ("round_pushpin", "\u{1F4CD}"),
    ("memo", "\u{1F4DD}"),
    ("clipboard", "\u{1F4CB}"),
    ("bulb", "\u{1F4A1}"),
    ("mag", "\u{1F50D}"),
    ("link", "\u{1F517}"),
    ("star", "\u{2B50}"),
    ("sparkles", "\u{2728}"),
    ("tada", "\u{1F389}"),
    ("balloon", "\u{1F388}"),
    ("gift", "\u{1F381}"),
    ("trophy", "\u{1F3C6}"),
    ("heart", "\u{2764}\u{FE0F}"),
    ("100", "\u{1F4AF}"),
    // Colours
    ("red_circle", "\u{1F534}"),
    ("orange_circle", "\u{1F7E0}"),
    ("yellow_circle", "\u{1F7E1}"),
    ("green_circle", "\u{1F7E2}"),
    ("blue_circle", "\u{1F535}"),
    ("purple_circle", "\u{1F7E3}"),
    ("white_circle", "\u{26AA}"),
    ("black_circle", "\u{26AB}"),
    // Arrows
    ("arrow_right", "\u{27A1}\u{FE0F}"),
    ("arrow_left", "\u{2B05}\u{FE0F}"),
    ("arrow_up", "\u{2B06}\u{FE0F}"),
    ("arrow_down", "\u{2B07}\u{FE0F}"),
    ("point_right", "\u{1F449}"),
    ("point_left", "\u{1F448}"),
    ("point_up", "\u{261D}\u{FE0F}"),
    ("point_down", "\u{1F447}"),
    // Time
    ("clock", "\u{1F552}"),
    ("alarm_clock", "\u{23F0}"),
    ("stopwatch", "\u{23F1}\u{FE0F}"),
    ("timer", "\u{23F2}\u{FE0F}"),
    ("hourglass", "\u{231B}"),
    ("hourglass_flowing_sand", "\u{23F3}"),
    ("calendar", "\u{1F4C6}"),
    ("date", "\u{1F4C5}"),
    // Weather
    ("sunny", "\u{2600}\u{FE0F}"),
    ("cloud", "\u{2601}\u{FE0F}"),
    ("partly_sunny", "\u{26C5}"),
    ("umbrella", "\u{2614}"),
    ("rain_cloud", "\u{1F327}\u{FE0F}"),
    ("thunder_cloud_and_rain", "\u{26C8}\u{FE0F}"),
    ("lightning", "\u{1F329}\u{FE0F}"),
    ("tornado", "\u{1F32A}\u{FE0F}"),
    ("fog", "\u{1F32B}\u{FE0F}"),
    ("snowflake", "\u{2744}\u{FE0F}"),
    ("snowman", "\u{26C4}"),
    ("ocean", "\u{1F30A}"),
    ("wind", "\u{1F4A8}"),
    ("dash", "\u{1F4A8}"),
    ("droplet", "\u{1F4A7}"),
    ("thermometer", "\u{1F321}\u{FE0F}"),
    ("rainbow", "\u{1F308}"),
    // People and gestures
    ("wave", "\u{1F44B}"),
    ("thumbsup", "\u{1F44D}"),
    ("+1", "\u{1F44D}"),
    ("thumbsdown", "\u{1F44E}"),
    ("-1", "\u{1F44E}"),
    ("clap", "\u{1F44F}"),
    ("pray", "\u{1F64F}"),
    ("raised_hand", "\u{270B}"),
    ("eyes", "\u{1F440}"),
    ("smile", "\u{1F604}"),
    ("slightly_smiling_face", "\u{1F642}"),
    ("grin", "\u{1F601}"),
    ("joy", "\u{1F602}"),
    ("wink", "\u{1F609}"),
    ("thinking", "\u{1F914}"),
    ("cry", "\u{1F622}"),
    ("shushing_face", "\u{1F92B}"),
    ("mask", "\u{1F637}"),
    ("runner", "\u{1F3C3}"),
    ("walking", "\u{1F6B6}"),
    ("wheelchair", "\u{267F}"),
    ("family", "\u{1F46A}"),
    ("baby", "\u{1F476}"),
    // Places and getting about
    ("door", "\u{1F6AA}"),
    ("house", "\u{1F3E0}"),
    ("school", "\u{1F3EB}"),
    ("hospital", "\u{1F3E5}"),
    ("office", "\u{1F3E2}"),
    ("church", "\u{26EA}"),
    ("factory", "\u{1F3ED}"),
    ("elevator", "\u{1F6D7}"),
    ("restroom", "\u{1F6BB}"),
    ("parking", "\u{1F17F}\u{FE0F}"),
    ("bus", "\u{1F68C}"),
    ("car", "\u{1F697}"),
    ("truck", "\u{1F69A}"),
    ("train", "\u{1F686}"),
    ("airplane", "\u{2708}\u{FE0F}"),
    ("bike", "\u{1F6B2}"),
    ("world_map", "\u{1F5FA}\u{FE0F}"),
    // The day
    ("coffee", "\u{2615}"),
    ("fork_and_knife", "\u{1F374}"),
    ("pizza", "\u{1F355}"),
    ("hamburger", "\u{1F354}"),
    ("cake", "\u{1F370}"),
    ("birthday", "\u{1F382}"),
    ("apple", "\u{1F34E}"),
    ("books", "\u{1F4DA}"),
    ("book", "\u{1F4D6}"),
    ("pencil", "\u{270F}\u{FE0F}"),
    ("mortar_board", "\u{1F393}"),
    ("musical_note", "\u{1F3B5}"),
    ("notes", "\u{1F3B6}"),
    ("soccer", "\u{26BD}"),
    ("basketball", "\u{1F3C0}"),
    ("football", "\u{1F3C8}"),
    ("baseball", "\u{26BE}"),
    ("swimmer", "\u{1F3CA}"),
    ("art", "\u{1F3A8}"),
    ("performing_arts", "\u{1F3AD}"),
    ("movie_camera", "\u{1F3A5}"),
    ("camera", "\u{1F4F7}"),
    ("christmas_tree", "\u{1F384}"),
    ("jack_o_lantern", "\u{1F383}"),
    ("fireworks", "\u{1F386}"),
    // Things
    ("telephone", "\u{260E}\u{FE0F}"),
    ("phone", "\u{260E}\u{FE0F}"),
    ("iphone", "\u{1F4F1}"),
    ("email", "\u{2709}\u{FE0F}"),
    ("envelope", "\u{2709}\u{FE0F}"),
    ("package", "\u{1F4E6}"),
    ("computer", "\u{1F4BB}"),
    ("printer", "\u{1F5A8}\u{FE0F}"),
    ("wrench", "\u{1F527}"),
    ("hammer", "\u{1F528}"),
    ("hammer_and_wrench", "\u{1F6E0}\u{FE0F}"),
    ("gear", "\u{2699}\u{FE0F}"),
    ("broom", "\u{1F9F9}"),
    ("wastebasket", "\u{1F5D1}\u{FE0F}"),
    ("recycle", "\u{267B}\u{FE0F}"),
    ("pill", "\u{1F48A}"),
    ("syringe", "\u{1F489}"),
    ("adhesive_bandage", "\u{1FA79}"),
    ("moneybag", "\u{1F4B0}"),
    ("credit_card", "\u{1F4B3}"),
    ("ticket", "\u{1F3AB}"),
    ("dog", "\u{1F436}"),
    ("cat", "\u{1F431}"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_known_shortcodes_only() {
        assert_eq!(expand(":fire: drill"), "\u{1F525} drill");
        assert_eq!(expand(":Fire::bell:"), "\u{1F525}\u{1F514}");
        assert_eq!(expand("ok :+1: :-1:"), "ok \u{1F44D} \u{1F44E}");
        // Times, ratios and names we don't have stay as typed.
        for s in [
            "at 10:30:00",
            "a:b:c",
            ":nope:",
            "::",
            ":",
            "mix 1:2:3 :",
            ":fire",
            "caf\u{e9}: :\u{e9}:",
        ] {
            assert_eq!(expand(s), s);
        }
        // An unknown name's closing colon can open a known one.
        assert_eq!(expand(":nope:fire:"), ":nope\u{1F525}");
        assert_eq!(expand(&format!(":{}:", "a".repeat(200))).len(), 202);
    }

    #[test]
    fn shortcodes_are_well_formed() {
        for (i, (name, emoji)) in SHORTCODES.iter().enumerate() {
            assert!(
                !name.is_empty()
                    && name.len() <= MAX_NAME
                    && name.bytes().all(|b| b.is_ascii_lowercase()
                        || b.is_ascii_digit()
                        || b"_+-".contains(&b)),
                "{name}"
            );
            assert!(
                !SHORTCODES[..i].iter().any(|(n, _)| n == name),
                "{name} is listed twice"
            );
            // Each is one emoji, and nothing is left of it when spoken.
            assert_eq!(count(emoji), 1, "{name}");
            assert_eq!(strip(emoji), "", "{name}");
            assert_eq!(expand(&format!(":{name}:")), *emoji);
        }
    }

    #[test]
    fn strips_emoji_for_speech() {
        assert_eq!(strip("\u{1F525} Fire drill \u{1F525}"), "Fire drill ");
        assert_eq!(strip("Fire \u{1F525} drill"), "Fire drill");
        assert_eq!(strip("fire\u{1F525}drill"), "fire drill");
        assert_eq!(strip("Lunch \u{1F355}!"), "Lunch!");
        assert_eq!(
            strip("Lunch\u{1F355}\u{1F354}, then class"),
            "Lunch, then class"
        );
        assert_eq!(strip("one\n\u{1F514}\ntwo"), "one\n\ntwo");
        assert_eq!(
            strip("Drill at 3 \u{1F6A8}\nEast stairs \u{27A1}\u{FE0F}"),
            "Drill at 3\nEast stairs "
        );
        // Joined emoji, skin tones, flags and selectors go as one.
        assert_eq!(
            strip("a \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} b \u{1F44D}\u{1F3FD} c \u{1F1EC}\u{1F1E7} d"),
            "a b c d"
        );
        assert_eq!(strip("\u{26A0}\u{FE0F} Wet floor"), "Wet floor");
        assert_eq!(strip("\u{a9}\u{FE0F} 2026"), "2026");
        assert_eq!(
            strip("\u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F} Alba"),
            "Alba"
        );
        // A keycap leaves its digit; plain symbols and other scripts stay.
        assert_eq!(strip("Gate 2\u{FE0F}\u{20E3}"), "Gate 2");
        assert_eq!(
            strip("\u{a9} 2026, 5 \u{d7} 3 \u{2192} 15"),
            "\u{a9} 2026, 5 \u{d7} 3 \u{2192} 15"
        );
        assert_eq!(
            strip("\u{645}\u{6cc}\u{200C}\u{62e}"),
            "\u{645}\u{6cc}\u{200C}\u{62e}"
        );
        assert_eq!(strip("\u{1F525}\u{1F525}"), "");
    }

    #[test]
    fn counts_what_a_reader_sees() {
        assert_eq!(count("abc"), 3);
        assert_eq!(count("\u{1F514}"), 1);
        assert_eq!(
            count("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}"),
            1
        );
        assert_eq!(count("\u{1F44D}\u{1F3FD}\u{26A0}\u{FE0F}"), 2);
        assert_eq!(count("\u{1F1EC}\u{1F1E7}\u{1F1EB}\u{1F1F7}"), 2);
        assert_eq!(count("1\u{FE0F}\u{20E3}"), 1);
        assert_eq!(
            count("\u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}"),
            1
        );
    }
}
