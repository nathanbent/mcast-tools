//! The page form's fields and the send request they make. No drawing here.

use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Text,
    Number,
    /// (value, label) pairs; the field's value is one of the values.
    Choice(Vec<(String, String)>),
    Toggle,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    pub value: String,
}

impl Field {
    fn new(key: &'static str, label: &'static str, kind: Kind, value: &str) -> Self {
        Self {
            key,
            label,
            kind,
            value: value.into(),
        }
    }

    /// The value as shown.
    pub fn display(&self) -> String {
        match &self.kind {
            Kind::Choice(opts) => opts
                .iter()
                .find(|(v, _)| *v == self.value)
                .map_or_else(|| "(none)".to_string(), |(_, l)| l.clone()),
            Kind::Toggle => if self.value == "true" { "[x]" } else { "[ ]" }.into(),
            _ => self.value.clone(),
        }
    }
}

fn choices(pairs: &[(&str, &str)]) -> Kind {
    Kind::Choice(
        pairs
            .iter()
            .map(|(v, l)| (v.to_string(), l.to_string()))
            .collect(),
    )
}

pub struct PageForm {
    pub fields: Vec<Field>,
    /// Index into `visible()`.
    pub selected: usize,
}

impl Default for PageForm {
    fn default() -> Self {
        Self::new()
    }
}

impl PageForm {
    pub fn new() -> Self {
        let fields = vec![
            Field::new("group", "Group", Kind::Text, "239.192.2.1:10000"),
            Field::new("name", "Name", Kind::Text, ""),
            Field::new(
                "source",
                "Source",
                choices(&[
                    ("file", "Audio file"),
                    ("capture", "Live input"),
                    ("tone", "Test tone"),
                ]),
                "file",
            ),
            Field::new("file", "File", Kind::Choice(Vec::new()), ""),
            Field::new("loop", "Loop", Kind::Toggle, "false"),
            Field::new(
                "device",
                "Input",
                Kind::Choice(vec![(String::new(), "Default input".into())]),
                "",
            ),
            Field::new("tone", "Frequency (Hz)", Kind::Number, "440"),
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
            Field::new("chime", "Chime first", Kind::Toggle, "true"),
            Field::new("ef", "Voice QoS (DSCP EF)", Kind::Toggle, "true"),
            Field::new("lead_in_ms", "Lead-in (ms)", Kind::Number, "800"),
            Field::new("tail_ms", "Tail (ms)", Kind::Number, "300"),
            Field::new("volume_db", "Volume (dB)", Kind::Number, "0"),
            Field::new("duration_s", "Stop after (s)", Kind::Number, ""),
        ];
        Self {
            fields,
            selected: 0,
        }
    }

    pub fn get(&self, key: &str) -> &str {
        &self
            .fields
            .iter()
            .find(|f| f.key == key)
            .expect("known field")
            .value
    }

    fn field_mut(&mut self, key: &str) -> &mut Field {
        self.fields
            .iter_mut()
            .find(|f| f.key == key)
            .expect("known field")
    }

    pub fn set(&mut self, key: &str, value: &str) {
        self.field_mut(key).value = value.into();
    }

    /// Indices of the fields shown for the current source.
    pub fn visible(&self) -> Vec<usize> {
        let source = self.get("source").to_string();
        (0..self.fields.len())
            .filter(|&i| match self.fields[i].key {
                "file" | "loop" => source == "file",
                "device" => source == "capture",
                "tone" => source == "tone",
                _ => true,
            })
            .collect()
    }

    pub fn current(&mut self) -> &mut Field {
        let v = self.visible();
        let i = v[self.selected.min(v.len() - 1)];
        &mut self.fields[i]
    }

    pub fn move_by(&mut self, delta: isize) {
        let n = self.visible().len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
    }

    /// Left/right on a choice or toggle.
    pub fn cycle(&mut self, delta: isize) {
        let f = self.current();
        match &f.kind {
            Kind::Choice(opts) if !opts.is_empty() => {
                let i = opts.iter().position(|(v, _)| *v == f.value).unwrap_or(0) as isize;
                f.value = opts[(i + delta).rem_euclid(opts.len() as isize) as usize]
                    .0
                    .clone();
            }
            Kind::Toggle => f.value = (f.value != "true").to_string(),
            _ => {}
        }
    }

    pub fn type_char(&mut self, c: char) {
        let f = self.current();
        match f.kind {
            Kind::Text if !c.is_control() => f.value.push(c),
            Kind::Number if c.is_ascii_digit() || c == '.' || (c == '-' && f.value.is_empty()) => {
                f.value.push(c)
            }
            Kind::Toggle if c == ' ' => f.value = (f.value != "true").to_string(),
            _ => {}
        }
    }

    pub fn backspace(&mut self) {
        let f = self.current();
        if matches!(f.kind, Kind::Text | Kind::Number) {
            f.value.pop();
        }
    }

    /// Keep the file and input choices in step with the server's lists.
    pub fn set_options(&mut self, media: &[String], inputs: &[String]) {
        let file = self.field_mut("file");
        file.kind = Kind::Choice(media.iter().map(|m| (m.clone(), m.clone())).collect());
        if !media.contains(&file.value) {
            file.value = media.first().cloned().unwrap_or_default();
        }
        let dev = self.field_mut("device");
        let mut opts = vec![(String::new(), "Default input".to_string())];
        opts.extend(inputs.iter().map(|d| (d.clone(), d.clone())));
        if !opts.iter().any(|(v, _)| *v == dev.value) {
            dev.value.clear();
        }
        dev.kind = Kind::Choice(opts);
    }

    /// Page a particular media file.
    pub fn use_file(&mut self, name: &str) {
        self.set("source", "file");
        self.set("file", name);
        self.selected = 0;
    }

    fn number(&self, key: &str) -> Result<Option<f64>, String> {
        let v = self.get(key).trim();
        if v.is_empty() {
            return Ok(None);
        }
        v.parse()
            .map(Some)
            .map_err(|_| format!("{key}: \"{v}\" isn't a number"))
    }

    /// The JSON body for `POST /senders`.
    pub fn request(&self) -> Result<Value, String> {
        let source = match self.get("source") {
            "file" if self.get("file").is_empty() => {
                return Err("upload or pick an audio file first (Media tab)".into())
            }
            "file" => {
                json!({"type": "file", "path": self.get("file"), "loop": self.get("loop") == "true"})
            }
            "capture" => {
                let d = self.get("device");
                json!({"type": "capture", "device": if d.is_empty() { Value::Null } else { d.into() }})
            }
            _ => json!({"type": "tone", "frequency": self.number("tone")?.unwrap_or(440.0)}),
        };
        let name = self.get("name").trim();
        Ok(json!({
            "group": self.get("group").trim(),
            "name": if name.is_empty() { Value::Null } else { name.into() },
            "source": source,
            "encoding": self.get("encoding"),
            "mode": self.get("mode"),
            "chime": if self.get("chime") == "true" { "default".into() } else { Value::Null },
            "tos": if self.get("ef") == "true" { 184.into() } else { Value::Null },
            "lead_in_ms": self.number("lead_in_ms")?.unwrap_or(0.0),
            "tail_ms": self.number("tail_ms")?.unwrap_or(0.0),
            "volume_db": self.number("volume_db")?.filter(|v| *v != 0.0),
            "duration_s": self.number("duration_s")?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_page_request() {
        let mut f = PageForm::new();
        assert!(f.request().is_err(), "no file yet");
        f.set_options(&["drill.wav".into()], &[]);
        let r = f.request().unwrap();
        assert_eq!(r["source"]["path"], "drill.wav");
        assert_eq!(r["encoding"], "pcmu");
        assert_eq!(r["chime"], "default");
        assert_eq!(r["tos"], 184);
        assert_eq!(r["volume_db"], Value::Null);
        assert_eq!(r["name"], Value::Null);
    }

    #[test]
    fn edits_fields_by_keyboard() {
        let mut f = PageForm::new();
        // Source -> tone: the file fields go and frequency appears.
        f.move_by(2);
        assert_eq!(f.current().key, "source");
        f.cycle(-1);
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
    fn rejects_bad_numbers() {
        let mut f = PageForm::new();
        f.set("source", "tone");
        f.set("lead_in_ms", "1.2.3");
        assert!(f.request().unwrap_err().contains("lead_in_ms"));
    }
}
