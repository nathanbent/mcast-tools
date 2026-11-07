//! Joining a text channel.

use super::{Field, Form, Kind};
use serde_json::{json, Value};

pub(super) fn fields() -> Vec<Field> {
    vec![
        Field::new("group", "Group", Kind::Text, "239.69.1.1:5004"),
        Field::new("name", "Your name", Kind::Text, ""),
        Field::new("listen", "Listen only", Kind::Toggle, "false"),
        Field::new("sap", "Announce (SAP)", Kind::Toggle, "false"),
    ]
}

/// The JSON body for `POST /texts`.
pub(super) fn request(f: &Form) -> Result<Value, String> {
    Ok(json!({
        "group": f.text_or_null("group"),
        "name": f.text_or_null("name"),
        "send": f.get("listen") != "true",
        "sap": f.get("sap") == "true",
    }))
}

#[cfg(test)]
mod tests {
    use super::super::FormKind;
    use super::*;

    #[test]
    fn builds_a_join_request() {
        let mut f = Form::new(FormKind::Text);
        let r = f.request().unwrap();
        assert_eq!(r["group"], "239.69.1.1:5004");
        assert_eq!(r["send"], true);
        assert_eq!(r["name"], Value::Null);
        f.set("listen", "true");
        f.set("name", " desk ");
        let r = f.request().unwrap();
        assert_eq!(
            (r["send"].clone(), r["name"].clone()),
            (json!(false), json!("desk"))
        );
    }
}
