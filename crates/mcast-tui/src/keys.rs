//! Key bindings. Each mode has its own handler.

use crate::app::{App, Level, Mode, ProbeField, Tab};
use crate::form::{is_audio, FormKind};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Shown by `?`: (keys, what they do). An empty description starts a section.
pub const HELP: &[(&str, &str)] = &[
    ("1-6, Tab", "switch tab"),
    ("↑↓ / j k", "select"),
    ("p", "page (open the page form)"),
    (
        "s, Del",
        "stop the selected job (Text tab: leave the channel)",
    ),
    ("x", "stop the job that blocked you"),
    ("r", "refresh everything (Media tab: receive files)"),
    ("q, Ctrl-C", "quit"),
    ("", ""),
    ("Text tab", ""),
    ("i, Enter", "type a message (Enter sends, Esc cancels)"),
    ("j", "join a channel"),
    ("[ ]", "previous / next channel"),
    ("", ""),
    ("Streams tab", ""),
    ("Enter", "listen (audio), join (text), receive (files)"),
    ("o", "choose the output device for listening"),
    ("", ""),
    ("Media tab", ""),
    ("Enter", "page the selected audio file"),
    ("f", "send the selected file over multicast"),
    ("r", "receive files"),
    ("u", "upload a local file"),
    ("d", "delete the selected file"),
    ("", ""),
    ("Probes tab", ""),
    (
        "u / s",
        "unicast copies / announced (SAP) streams on or off",
    ),
    ("l / g / i", "set the label / groups / report interval"),
    ("D", "back to the default configuration"),
    ("f, Del", "forget the probe"),
    ("", ""),
    ("Forms", ""),
    ("↑↓ / Tab", "move between fields"),
    ("←→ / Space", "change a choice or checkbox"),
    ("Enter", "start · Esc closes"),
];

pub fn handle(app: &mut App, key: KeyEvent) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    match app.mode.clone() {
        Mode::Normal => normal(app, key),
        Mode::Form(kind) => form(app, key, kind),
        // Enter sends and stays ready for the next message.
        Mode::Compose(text) if key.code == KeyCode::Enter => {
            app.say_text(text);
            app.mode = Mode::Compose(String::new());
        }
        Mode::Compose(text) => line(app, key, text, Mode::Compose, |_, _| {}),
        Mode::Upload(path) => line(app, key, path, Mode::Upload, |app, p| {
            if !p.trim().is_empty() {
                app.upload(p)
            }
        }),
        Mode::ProbeInput { id, field, text } => probe_input(app, key, id, field, text),
        Mode::ConfirmForget(id) => {
            if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                app.forget_probe(id);
            }
            app.mode = Mode::Normal;
        }
        Mode::ConfirmDelete(name) => {
            if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                app.delete_media(name);
            }
            app.mode = Mode::Normal;
        }
        Mode::Help => app.mode = Mode::Normal,
    }
}

fn select(app: &mut App, delta: isize) {
    let n = app.rows();
    if n == 0 {
        return;
    }
    let i = app.selected() as isize + delta;
    app.selected[app.tab.index()] = i.clamp(0, n as isize - 1) as usize;
    if app.tab == Tab::Probes {
        app.want_reports();
    }
}

fn switch(app: &mut App, tab: Tab) {
    app.tab = tab;
    if tab == Tab::Probes {
        app.want_reports();
    }
}

fn open(app: &mut App, kind: FormKind) {
    app.forms.get_mut(kind).selected = 0;
    app.mode = Mode::Form(kind);
}

fn normal(app: &mut App, key: KeyEvent) {
    // Per-tab keys first, so a tab can use a letter differently.
    if tab_key(app, key) {
        return;
    }
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.quit = true,
        KeyCode::Char('?') => app.mode = Mode::Help,
        KeyCode::Char(c @ '1'..='6') => switch(app, Tab::ALL[c as usize - '1' as usize]),
        KeyCode::Tab => switch(app, Tab::ALL[(app.tab.index() + 1) % Tab::ALL.len()]),
        KeyCode::BackTab => switch(
            app,
            Tab::ALL[(app.tab.index() + Tab::ALL.len() - 1) % Tab::ALL.len()],
        ),
        KeyCode::Up | KeyCode::Char('k') => select(app, -1),
        KeyCode::Down | KeyCode::Char('j') => select(app, 1),
        KeyCode::PageUp => select(app, -10),
        KeyCode::PageDown => select(app, 10),
        KeyCode::Char('p') => open(app, FormKind::Page),
        KeyCode::Char('r') => {
            app.refresh_all();
            app.say("Refreshed", Level::Info);
        }
        KeyCode::Char('x') => match app.conflict.take() {
            Some(id) => app.stop(id),
            None => app.say("nothing is in the way", Level::Info),
        },
        _ => {}
    }
}

/// Keys that mean something on the current tab. Returns whether it was used.
fn tab_key(app: &mut App, key: KeyEvent) -> bool {
    let i = app.selected();
    match (app.tab, key.code) {
        (Tab::Jobs, KeyCode::Char('s') | KeyCode::Delete) => {
            if let Some(j) = app.job_rows().get(i).filter(|j| j.active()).map(|j| j.id) {
                app.stop(j);
            }
        }
        (Tab::Text, KeyCode::Char('i') | KeyCode::Enter) => match app.current_channel() {
            Some(j) if j.text_sender() => app.mode = Mode::Compose(String::new()),
            Some(_) => app.say("this channel only listens", Level::Info),
            None => open(app, FormKind::Text),
        },
        (Tab::Text, KeyCode::Char('j')) => open(app, FormKind::Text),
        (Tab::Text, KeyCode::Char('[')) => app.next_channel(-1),
        (Tab::Text, KeyCode::Char(']')) => app.next_channel(1),
        (Tab::Text, KeyCode::Char('s') | KeyCode::Delete) => {
            if let Some(id) = app.current_channel().map(|j| j.id) {
                app.stop(id);
            }
        }
        (Tab::Streams, KeyCode::Enter) => {
            if let Some(s) = app.streams.get(i).cloned() {
                app.use_stream(&s);
            }
        }
        (Tab::Streams, KeyCode::Char('o')) => app.next_output(),
        (Tab::Media, KeyCode::Enter) => {
            let Some(name) = app.media.get(i).map(|m| m.name.clone()) else {
                return true;
            };
            if !is_audio(&name) {
                app.say("only audio can be paged; f sends it as a file", Level::Info);
                return true;
            }
            app.forms.page.set("source", "file");
            app.forms.page.set("file", &name);
            open(app, FormKind::Page);
        }
        (Tab::Media, KeyCode::Char('f')) => {
            if let Some(name) = app.media.get(i).map(|m| m.name.clone()) {
                app.forms.send.set("file", &name);
            }
            open(app, FormKind::SendFiles);
        }
        (Tab::Media, KeyCode::Char('r')) => open(app, FormKind::ReceiveFiles),
        (Tab::Media, KeyCode::Char('u')) => app.mode = Mode::Upload(String::new()),
        (Tab::Media, KeyCode::Char('d') | KeyCode::Delete) => {
            if let Some(m) = app.media.get(i) {
                app.mode = Mode::ConfirmDelete(m.name.clone());
            }
        }
        (Tab::Probes, code) => return probe_key(app, code),
        _ => return false,
    }
    true
}

/// Probes tab keys. Returns whether it was used.
fn probe_key(app: &mut App, code: KeyCode) -> bool {
    let Some(p) = app.selected_probe() else {
        return false;
    };
    let id = p.id.clone();
    let prompt = |field, text: String| Mode::ProbeInput {
        id: id.clone(),
        field,
        text,
    };
    match code {
        KeyCode::Char('u') => {
            app.toggle_probe("unicast", "unicast copies on", "unicast copies off")
        }
        KeyCode::Char('s') => app.toggle_probe(
            "sap",
            "watching announced streams",
            "not watching announced streams",
        ),
        KeyCode::Char('l') => {
            app.mode = prompt(ProbeField::Label, p.label().unwrap_or("").to_string())
        }
        KeyCode::Char('g') => app.mode = prompt(ProbeField::Groups, p.groups().join(" ")),
        KeyCode::Char('i') => {
            app.mode = prompt(ProbeField::Interval, format!("{}", p.interval_s()))
        }
        KeyCode::Char('D') if p.custom_config => app.reset_probe(),
        KeyCode::Char('D') => app.say("it already uses the defaults", Level::Info),
        KeyCode::Char('f') | KeyCode::Delete => app.mode = Mode::ConfirmForget(id),
        _ => return false,
    }
    true
}

fn probe_input(app: &mut App, key: KeyEvent, id: String, field: ProbeField, mut text: String) {
    match key.code {
        KeyCode::Esc => app.mode = Mode::Normal,
        KeyCode::Enter => {
            app.mode = Mode::Normal;
            // The selection may have moved under an event; only edit the probe asked about.
            if app.selected_probe().is_some_and(|p| p.id == id) {
                app.set_probe_field(field, text);
            }
        }
        KeyCode::Backspace => {
            text.pop();
            app.mode = Mode::ProbeInput { id, field, text };
        }
        KeyCode::Char(c) => {
            text.push(c);
            app.mode = Mode::ProbeInput { id, field, text };
        }
        _ => {}
    }
}

fn form(app: &mut App, key: KeyEvent, kind: FormKind) {
    let f = app.forms.get_mut(kind);
    match key.code {
        KeyCode::Esc => app.mode = Mode::Normal,
        KeyCode::Enter if !app.busy => app.submit(kind),
        KeyCode::Up | KeyCode::BackTab => f.move_by(-1),
        KeyCode::Down | KeyCode::Tab => f.move_by(1),
        KeyCode::Left => f.cycle(-1),
        KeyCode::Right => f.cycle(1),
        KeyCode::Backspace => f.backspace(),
        KeyCode::Char(c) => f.type_char(c),
        _ => {}
    }
}

/// A one-line text input: Enter finishes it, Esc cancels.
fn line(
    app: &mut App,
    key: KeyEvent,
    mut text: String,
    mode: fn(String) -> Mode,
    done: impl FnOnce(&mut App, String),
) {
    match key.code {
        KeyCode::Esc => app.mode = Mode::Normal,
        KeyCode::Enter => {
            app.mode = Mode::Normal;
            done(app, text);
        }
        KeyCode::Backspace => {
            text.pop();
            app.mode = mode(text);
        }
        KeyCode::Char(c) => {
            text.push(c);
            app.mode = mode(text);
        }
        _ => {}
    }
}
