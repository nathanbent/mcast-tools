//! A phonebook entry: a group saved under a name and description.

use super::{Field, Form, Kind};
use serde_json::{json, Value};

pub(super) fn fields() -> Vec<Field> {
    vec![
        Field::new("name", "Name", Kind::Text, ""),
        Field::new("group", "Group", Kind::Text, ""),
        Field::new("description", "Description", Kind::Text, ""),
    ]
}

/// The JSON body for `PUT /phonebook/{name}`. The name goes in the path
/// (the old one, when renaming), and in the body.
pub(super) fn request(f: &Form) -> Result<Value, String> {
    let name = f.get("name").trim();
    if name.is_empty() {
        return Err("give the entry a name".into());
    }
    let group = f.get("group").trim();
    if group.is_empty() {
        return Err("give the group and port, e.g. 239.192.2.1:10000".into());
    }
    Ok(json!({
        "name": name,
        "group": group,
        "description": f.get("description").trim(),
    }))
}

#[cfg(test)]
mod tests {
    use super::super::FormKind;
    use super::*;

    #[test]
    fn builds_an_entry() {
        let mut f = Form::new(FormKind::Book);
        assert!(f.request().is_err());
        f.set("name", " zone-1 ");
        assert!(f.request().is_err());
        f.set("group", "239.192.2.1:10000");
        f.set("description", "Ground floor ");
        assert_eq!(
            f.request().unwrap(),
            json!({"name": "zone-1", "group": "239.192.2.1:10000", "description": "Ground floor"})
        );
    }
}
