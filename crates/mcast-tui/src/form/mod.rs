//! Forms: fields edited from the keyboard, and the request each builds.
//! No drawing here. Each form's fields and request live in its own module.

mod files;
mod page;
mod text;

use serde_json::Value;

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
    /// Shown only while the field `.0` has one of the values `.1`.
    pub when: Option<(&'static str, &'static [&'static str])>,
}

impl Field {
    pub(crate) fn new(key: &'static str, label: &'static str, kind: Kind, value: &str) -> Self {
        Self {
            key,
            label,
            kind,
            value: value.into(),
            when: None,
        }
    }

    pub(crate) fn when(mut self, key: &'static str, values: &'static [&'static str]) -> Self {
        self.when = Some((key, values));
        self
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

pub(crate) fn choices(pairs: &[(&str, &str)]) -> Kind {
    Kind::Choice(
        pairs
            .iter()
            .map(|(v, l)| (v.to_string(), l.to_string()))
            .collect(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormKind {
    Page,
    Text,
    SendFiles,
    ReceiveFiles,
}

impl FormKind {
    pub fn title(self) -> &'static str {
        match self {
            FormKind::Page => "Page",
            FormKind::Text => "Join a text channel",
            FormKind::SendFiles => "Send a file",
            FormKind::ReceiveFiles => "Receive files",
        }
    }

    pub fn submit(self) -> &'static str {
        match self {
            FormKind::Page => "starts the page",
            FormKind::Text => "joins",
            FormKind::SendFiles => "sends",
            FormKind::ReceiveFiles => "starts receiving",
        }
    }
}

pub struct Form {
    pub kind: FormKind,
    pub fields: Vec<Field>,
    /// Index into `visible()`.
    pub selected: usize,
}

impl Form {
    pub fn new(kind: FormKind) -> Self {
        let fields = match kind {
            FormKind::Page => page::fields(),
            FormKind::Text => text::fields(),
            FormKind::SendFiles => files::send_fields(),
            FormKind::ReceiveFiles => files::receive_fields(),
        };
        Self {
            kind,
            fields,
            selected: 0,
        }
    }

    /// The JSON body this form posts.
    pub fn request(&self) -> Result<Value, String> {
        match self.kind {
            FormKind::Page => page::request(self),
            FormKind::Text => text::request(self),
            FormKind::SendFiles => files::send_request(self),
            FormKind::ReceiveFiles => files::receive_request(self),
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

    pub fn has(&self, key: &str) -> bool {
        self.fields.iter().any(|f| f.key == key)
    }

    /// Indices of the fields shown, given the others' values.
    pub fn visible(&self) -> Vec<usize> {
        (0..self.fields.len())
            .filter(|&i| match self.fields[i].when {
                Some((key, values)) => values.contains(&self.get(key)),
                None => true,
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

    /// Replace a choice field's options, keeping its value if still offered
    /// (else the first option).
    pub fn set_choices(&mut self, key: &str, opts: Vec<(String, String)>) {
        if !self.has(key) {
            return;
        }
        let f = self.field_mut(key);
        if !opts.iter().any(|(v, _)| *v == f.value) {
            f.value = opts.first().map(|(v, _)| v.clone()).unwrap_or_default();
        }
        f.kind = Kind::Choice(opts);
    }

    pub(crate) fn number(&self, key: &str) -> Result<Option<f64>, String> {
        let v = self.get(key).trim();
        if v.is_empty() {
            return Ok(None);
        }
        v.parse()
            .map(Some)
            .map_err(|_| format!("{key}: \"{v}\" isn't a number"))
    }

    /// A text field, or null when blank.
    pub(crate) fn text_or_null(&self, key: &str) -> Value {
        let v = self.get(key).trim();
        if v.is_empty() {
            Value::Null
        } else {
            v.into()
        }
    }
}

/// All the forms, each keeping its values while closed.
pub struct Forms {
    pub page: Form,
    pub text: Form,
    pub send: Form,
    pub receive: Form,
}

impl Default for Forms {
    fn default() -> Self {
        Self {
            page: Form::new(FormKind::Page),
            text: Form::new(FormKind::Text),
            send: Form::new(FormKind::SendFiles),
            receive: Form::new(FormKind::ReceiveFiles),
        }
    }
}

impl Forms {
    pub fn get(&self, kind: FormKind) -> &Form {
        match kind {
            FormKind::Page => &self.page,
            FormKind::Text => &self.text,
            FormKind::SendFiles => &self.send,
            FormKind::ReceiveFiles => &self.receive,
        }
    }

    pub fn get_mut(&mut self, kind: FormKind) -> &mut Form {
        match kind {
            FormKind::Page => &mut self.page,
            FormKind::Text => &mut self.text,
            FormKind::SendFiles => &mut self.send,
            FormKind::ReceiveFiles => &mut self.receive,
        }
    }

    /// Keep file and input choices in step with the server's lists.
    pub fn set_options(&mut self, media: &[String], inputs: &[String]) {
        let pair = |v: &String| (v.clone(), v.clone());
        // Only audio can be paged; anything can be sent.
        let audio: Vec<_> = media.iter().filter(|m| is_audio(m)).map(pair).collect();
        self.page.set_choices("file", audio);
        let mut devices = vec![(String::new(), "Default input".to_string())];
        devices.extend(inputs.iter().map(pair));
        self.page.set_choices("device", devices);
        self.send
            .set_choices("file", media.iter().map(pair).collect());
    }
}

pub fn is_audio(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "wav" | "mp3" | "flac" | "ogg" | "oga" | "m4a" | "aac" | "mp4" | "alac"
    )
}
