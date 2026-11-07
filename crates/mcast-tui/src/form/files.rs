//! Sending a media file, and receiving files.

use super::{Field, Form, Kind};
use serde_json::{json, Value};

pub(super) fn send_fields() -> Vec<Field> {
    vec![
        Field::new("file", "File", Kind::Choice(Vec::new()), ""),
        Field::new("group", "Group", Kind::Text, "239.69.2.1:5004"),
        Field::new("rate", "Rate (kbit/s)", Kind::Number, "2000"),
        Field::new("loop", "Keep sending", Kind::Toggle, "false"),
        Field::new("name", "Name (SAP)", Kind::Text, ""),
    ]
}

/// The JSON body for `POST /file-senders`.
pub(super) fn send_request(f: &Form) -> Result<Value, String> {
    let file = f.get("file");
    if file.is_empty() {
        return Err("upload a file first (Media tab, u)".into());
    }
    Ok(json!({
        "files": [file],
        "group": f.get("group").trim(),
        "rate_kbps": f.number("rate")?.unwrap_or(2000.0) as u64,
        "loop": f.get("loop") == "true",
        "name": f.text_or_null("name"),
    }))
}

pub(super) fn receive_fields() -> Vec<Field> {
    vec![
        Field::new("group", "Group", Kind::Text, "239.69.2.1:5004"),
        Field::new("max", "Largest file (MB)", Kind::Number, ""),
    ]
}

/// The JSON body for `POST /file-receivers`.
pub(super) fn receive_request(f: &Form) -> Result<Value, String> {
    Ok(json!({
        "group": f.text_or_null("group"),
        "max_size_mb": f.number("max")?.map(|m| m as u64),
    }))
}

#[cfg(test)]
mod tests {
    use super::super::FormKind;
    use super::*;

    #[test]
    fn builds_send_and_receive_requests() {
        let mut f = Form::new(FormKind::SendFiles);
        assert!(f.request().is_err(), "no file yet");
        f.set_choices("file", vec![("map.png".into(), "map.png".into())]);
        f.set("rate", "500");
        let r = f.request().unwrap();
        assert_eq!(r["files"], json!(["map.png"]));
        assert_eq!(r["rate_kbps"], 500);
        let r = Form::new(FormKind::ReceiveFiles).request().unwrap();
        assert_eq!(r["max_size_mb"], Value::Null, "the server picks its cap");
    }
}
