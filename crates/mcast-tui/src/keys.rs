//! Key bindings. Each mode has its own handler.

use crate::app::{App, Level, Mode, ProbeField, Tab};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Shown by `?`: (keys, what they do).
pub const HELP: &[(&str, &str)] = &[
    ("1-5, Tab", "switch tab"),
    ("↑↓ / j k", "select"),
    ("p", "page (open the form)"),
    ("s, Del", "stop the selected job"),
    ("x", "stop the job that blocked your page"),
    ("Enter", "Streams: listen · Media: page this file"),
    ("o", "Streams: choose the output device"),
    ("u", "Media: upload a local file"),
    ("d", "Media: delete the selected file"),
    ("u / s", "Probes: unicast copies / SAP streams on or off"),
    ("l / g / i", "Probes: set label / groups / report interval"),
    ("D", "Probes: back to the default configuration"),
    ("f", "Probes: forget the probe"),
    ("r", "refresh everything"),
    ("q, Ctrl-C", "quit"),
    ("", ""),
    ("Page form", ""),
    ("↑↓ / Tab", "move between fields"),
    ("←→ / Space", "change a choice or checkbox"),
    ("Enter", "start the page"),
    ("Esc", "close the form"),
];

pub fn handle(app: &mut App, key: KeyEvent) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    match app.mode.clone() {
        Mode::Normal => normal(app, key),
        Mode::Page => page(app, key),
        Mode::Upload(path) => upload(app, key, path),
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

fn normal(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.quit = true,
        KeyCode::Char('?') => app.mode = Mode::Help,
        KeyCode::Char(c @ '1'..='5') => switch(app, Tab::ALL[c as usize - '1' as usize]),
        KeyCode::Tab => switch(app, Tab::ALL[(app.tab.index() + 1) % Tab::ALL.len()]),
        KeyCode::BackTab => switch(
            app,
            Tab::ALL[(app.tab.index() + Tab::ALL.len() - 1) % Tab::ALL.len()],
        ),
        KeyCode::Up | KeyCode::Char('k') => select(app, -1),
        KeyCode::Down | KeyCode::Char('j') => select(app, 1),
        KeyCode::PageUp => select(app, -10),
        KeyCode::PageDown => select(app, 10),
        KeyCode::Char('p') => app.mode = Mode::Page,
        KeyCode::Char('r') => {
            app.refresh_all();
            app.say("Refreshed", Level::Info);
        }
        KeyCode::Char('x') => match app.conflict.take() {
            Some(id) => app.stop(id),
            None => app.say("nothing blocked a page", Level::Info),
        },
        _ => tab_key(app, key),
    }
}

fn tab_key(app: &mut App, key: KeyEvent) {
    let i = app.selected();
    match (app.tab, key.code) {
        (Tab::Jobs, KeyCode::Char('s') | KeyCode::Delete) => {
            if let Some(j) = app.job_rows().get(i).filter(|j| j.active()).map(|j| j.id) {
                app.stop(j);
            }
        }
        (Tab::Streams, KeyCode::Enter) => {
            if let Some(s) = app.streams.get(i).cloned() {
                app.listen(&s);
            }
        }
        (Tab::Streams, KeyCode::Char('o')) => app.next_output(),
        (Tab::Media, KeyCode::Enter) => {
            if let Some(m) = app.media.get(i) {
                let name = m.name.clone();
                app.form.use_file(&name);
                app.mode = Mode::Page;
            }
        }
        (Tab::Media, KeyCode::Char('u')) => app.mode = Mode::Upload(String::new()),
        (Tab::Probes, code) => probe_key(app, code),
        (Tab::Media, KeyCode::Char('d') | KeyCode::Delete) => {
            if let Some(m) = app.media.get(i) {
                app.mode = Mode::ConfirmDelete(m.name.clone());
            }
        }
        _ => {}
    }
}

fn probe_key(app: &mut App, code: KeyCode) {
    let Some(p) = app.selected_probe() else {
        return;
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
        _ => {}
    }
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

fn page(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.mode = Mode::Normal,
        KeyCode::Enter if !app.busy => app.submit_page(),
        KeyCode::Up | KeyCode::BackTab => app.form.move_by(-1),
        KeyCode::Down | KeyCode::Tab => app.form.move_by(1),
        KeyCode::Left => app.form.cycle(-1),
        KeyCode::Right => app.form.cycle(1),
        KeyCode::Backspace => app.form.backspace(),
        KeyCode::Char(c) => app.form.type_char(c),
        _ => {}
    }
}

fn upload(app: &mut App, key: KeyEvent, mut path: String) {
    match key.code {
        KeyCode::Esc => app.mode = Mode::Normal,
        KeyCode::Enter => {
            app.mode = Mode::Normal;
            if !path.trim().is_empty() {
                app.upload(path);
            }
        }
        KeyCode::Backspace => {
            path.pop();
            app.mode = Mode::Upload(path);
        }
        KeyCode::Char(c) => {
            path.push(c);
            app.mode = Mode::Upload(path);
        }
        _ => {}
    }
}
