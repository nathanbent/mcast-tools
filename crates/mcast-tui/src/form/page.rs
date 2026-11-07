//! The page form: what to send, where, and how it's processed.

use super::{choices, Field, Form, Kind};
use crate::model::Chime;
use serde_json::{json, Value};

/// Chime choices until the server's list arrives: off, the server's
/// default, or one of the built-ins every version has.
const CHIMES: [(&str, &str); 8] = [
    ("none", "Off"),
    ("default", "Default"),
    ("rising", "Rising"),
    ("falling", "Falling"),
    ("ding-dong", "Ding dong"),
    ("arpeggio", "Arpeggio"),
    ("bell", "Bell"),
    ("alert", "Alert"),
];

/// Chime choices from the server's list: off, the default, then the
/// built-ins it doesn't hide, plus `current` if it's hidden (so a choice made
/// before it was hidden isn't lost).
pub(super) fn chime_choices(chimes: &[Chime], current: &str) -> Vec<(String, String)> {
    let mut opts: Vec<(String, String)> = CHIMES[..2]
        .iter()
        .map(|(v, l)| (v.to_string(), l.to_string()))
        .collect();
    opts.extend(
        chimes
            .iter()
            .filter(|c| !c.hidden || c.name == current)
            .map(|c| (c.name.clone(), chime_label(c))),
    );
    if !opts.iter().any(|(v, _)| v == current) && !current.is_empty() {
        opts.push((current.to_string(), current.to_string()));
    }
    opts
}

/// "Church bell (bells)": the name, readable, and its category.
fn chime_label(c: &Chime) -> String {
    let mut name = c.name.replace('-', " ");
    if let Some(first) = name.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    let category = match c.category.as_str() {
        "chime" => "chimes",
        "bell" => "bells",
        "tone" => "tones",
        "alert" => "alerts",
        other => other,
    };
    format!("{name} ({category})")
}

fn chime(f: &Form, key: &str) -> Value {
    match f.get(key) {
        "none" | "" => Value::Null,
        name => name.into(),
    }
}

pub(super) fn fields() -> Vec<Field> {
    vec![
        Field::new("group", "Group", Kind::Text, "239.192.2.1:10000"),
        Field::new("name", "Name", Kind::Text, ""),
        Field::new(
            "source",
            "Source",
            choices(&[
                ("file", "Audio file"),
                ("capture", "Live input"),
                ("tone", "Test tone"),
                ("pad", "Chord pad"),
                ("chime", "Chime only"),
            ]),
            "file",
        ),
        Field::new("file", "File", Kind::Choice(Vec::new()), "").when("source", &["file"]),
        Field::new("loop", "Loop", Kind::Toggle, "false").when("source", &["file"]),
        Field::new(
            "device",
            "Input",
            Kind::Choice(vec![(String::new(), "Default input".into())]),
            "",
        )
        .when("source", &["capture"]),
        Field::new("tone", "Frequency (Hz)", Kind::Number, "440").when("source", &["tone"]),
        Field::new(
            "encoding",
            "Codec",
            choices(&[
                ("pcmu", "G.711 µ-law"),
                ("pcma", "G.711 A-law"),
                ("g722", "G.722"),
                ("opus", "Opus"),
                ("l16", "L16"),
                ("l24", "L24"),
            ]),
            "pcmu",
        ),
        Field::new(
            "mode",
            "Processing",
            choices(&[("voice", "Voice"), ("music", "Music"), ("none", "None")]),
            "voice",
        ),
        Field::new("chime", "Chime first", choices(&CHIMES), "default"),
        Field::new("end_chime", "Chime at end", choices(&CHIMES), "none"),
        Field::new("ef", "Voice QoS (DSCP EF)", Kind::Toggle, "true"),
        Field::new("lead_in_ms", "Lead-in (ms)", Kind::Number, "800"),
        Field::new("tail_ms", "Tail (ms)", Kind::Number, "300"),
        Field::new("volume_db", "Volume (dB)", Kind::Number, "0"),
        Field::new("duration_s", "Stop after (s)", Kind::Number, ""),
    ]
}

/// The JSON body for `POST /senders`.
pub(super) fn request(f: &Form) -> Result<Value, String> {
    let source = match f.get("source") {
        "file" if f.get("file").is_empty() => {
            return Err("upload or pick an audio file first (Media tab)".into())
        }
        "file" => {
            json!({"type": "file", "path": f.get("file"), "loop": f.get("loop") == "true"})
        }
        "capture" => {
            let d = f.get("device");
            json!({"type": "capture", "device": if d.is_empty() { Value::Null } else { d.into() }})
        }
        "pad" => json!({"type": "pad"}),
        "chime" => json!({"type": "chime"}),
        _ => json!({"type": "tone", "frequency": f.number("tone")?.unwrap_or(440.0)}),
    };
    Ok(json!({
        "group": f.get("group").trim(),
        "name": f.text_or_null("name"),
        "source": source,
        "encoding": f.get("encoding"),
        "mode": f.get("mode"),
        "chime": chime(f, "chime"),
        "end_chime": chime(f, "end_chime"),
        "tos": if f.get("ef") == "true" { 184.into() } else { Value::Null },
        "lead_in_ms": f.number("lead_in_ms")?.unwrap_or(0.0),
        "tail_ms": f.number("tail_ms")?.unwrap_or(0.0),
        "volume_db": f.number("volume_db")?.filter(|v| *v != 0.0),
        "duration_s": f.number("duration_s")?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_page_request() {
        let mut f = Form::new(crate::form::FormKind::Page);
        assert!(f.request().is_err(), "no file yet");
        f.set_choices("file", vec![("drill.wav".into(), "drill.wav".into())]);
        let r = f.request().unwrap();
        assert_eq!(r["source"]["path"], "drill.wav");
        assert_eq!(r["encoding"], "pcmu");
        assert_eq!(r["chime"], "default");
        assert_eq!(r["end_chime"], Value::Null);
        f.set("chime", "ding-dong");
        f.set("end_chime", "falling");
        let r = f.request().unwrap();
        assert_eq!(r["chime"], "ding-dong");
        assert_eq!(r["end_chime"], "falling");
        assert_eq!(r["tos"], 184);
        assert_eq!(r["volume_db"], Value::Null);
        assert_eq!(r["name"], Value::Null);
    }

    #[test]
    fn edits_fields_by_keyboard() {
        let mut f = Form::new(crate::form::FormKind::Page);
        // Source -> tone: the file fields go and frequency appears.
        f.move_by(2);
        assert_eq!(f.current().key, "source");
        f.cycle(-3);
        assert_eq!(f.get("source"), "tone");
        f.move_by(1);
        assert_eq!(f.current().key, "tone");
        f.backspace();
        f.type_char('x'); // ignored in numbers
        f.type_char('0');
        assert_eq!(f.get("tone"), "440");
        assert_eq!(f.request().unwrap()["source"]["frequency"], 440.0);
        // Wraps around.
        f.selected = 0;
        f.move_by(-1);
        assert_eq!(f.current().key, "duration_s");
        f.type_char('5');
        assert_eq!(f.request().unwrap()["duration_s"], 5.0);
    }

    #[test]
    fn pad_needs_no_other_fields() {
        let mut f = Form::new(crate::form::FormKind::Page);
        f.move_by(2);
        f.cycle(-2);
        assert_eq!(f.get("source"), "pad");
        let keys: Vec<&str> = f.visible().iter().map(|&i| f.fields[i].key).collect();
        assert!(
            !keys
                .iter()
                .any(|k| ["file", "loop", "device", "tone"].contains(k)),
            "{keys:?}"
        );
        assert_eq!(f.request().unwrap()["source"], json!({"type": "pad"}));
    }

    #[test]
    fn server_chimes_replace_the_list_but_keep_a_hidden_choice() {
        let chime = |name: &str, category: &str, hidden: bool| Chime {
            name: name.into(),
            category: category.into(),
            description: String::new(),
            hidden,
        };
        let list = [
            chime("rising", "chime", false),
            chime("church-bell", "bell", false),
            chime("klaxon", "alert", true),
            chime("cowbell", "bell", true),
        ];
        let mut forms = super::super::Forms::default();
        forms.page.set("end_chime", "klaxon");
        forms.set_chimes(&list);
        let Kind::Choice(opts) = &forms
            .page
            .fields
            .iter()
            .find(|f| f.key == "chime")
            .unwrap()
            .kind
        else {
            panic!("chime is a choice")
        };
        let values: Vec<_> = opts.iter().map(|(v, _)| v.as_str()).collect();
        assert_eq!(values, ["none", "default", "rising", "church-bell"]);
        assert_eq!(opts[3].1, "Church bell (bells)");
        assert_eq!(forms.page.get("chime"), "default");
        // Hidden, but already chosen: still offered, still chosen.
        assert_eq!(forms.page.get("end_chime"), "klaxon");
    }

    #[test]
    fn chime_only_needs_no_file() {
        let mut f = Form::new(crate::form::FormKind::Page);
        f.set("source", "chime");
        f.set("chime", "none");
        f.set("end_chime", "bell");
        let r = f.request().unwrap();
        assert_eq!(r["source"], json!({"type": "chime"}));
        assert_eq!(r["chime"], Value::Null);
        assert_eq!(r["end_chime"], "bell");
    }

    #[test]
    fn rejects_bad_numbers() {
        let mut f = Form::new(crate::form::FormKind::Page);
        f.set("source", "tone");
        f.set("lead_in_ms", "1.2.3");
        assert!(f.request().unwrap_err().contains("lead_in_ms"));
    }
}
