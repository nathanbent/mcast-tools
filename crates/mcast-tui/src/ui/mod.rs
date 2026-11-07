//! Drawing. One module per tab, plus the overlays (page form, prompts, help).

mod jobs;
mod media;
mod monitor;
mod page;
mod probes;
mod streams;

use crate::app::{App, Level, Mode, Tab};
use crate::keys::HELP;
use crate::model::Health;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Tabs, Wrap};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(f.area());
    draw_header(f, app, header);
    let inner = body.inner(ratatui::layout::Margin::new(1, 0));
    match app.tab {
        Tab::Jobs => jobs::draw(f, app, inner),
        Tab::Streams => streams::draw(f, app, inner),
        Tab::Monitor => monitor::draw(f, app, inner),
        Tab::Media => media::draw(f, app, inner),
        Tab::Probes => probes::draw(f, app, inner),
    }
    draw_footer(f, app, footer);
    match &app.mode {
        Mode::Page => page::draw(f, app),
        Mode::Upload(path) => prompt(
            f,
            "Upload",
            vec![
                Line::from("Path of a local audio file (WAV, MP3, FLAC, Ogg, AAC/M4A):"),
                Line::from(vec![
                    Span::raw(path.clone()),
                    Span::raw("▏").fg(Color::Cyan),
                ]),
                Line::from(""),
                Line::from("Enter uploads · Esc cancels").dim(),
            ],
        ),
        Mode::ConfirmDelete(name) => prompt(
            f,
            "Delete",
            vec![Line::from(format!(
                "Delete \"{name}\" from the server? (y/n)"
            ))],
        ),
        Mode::ProbeInput { id, field, text } => prompt(
            f,
            id,
            vec![
                Line::from(field.prompt()),
                Line::from(vec![
                    Span::raw(text.clone()),
                    Span::raw("▏").fg(Color::Cyan),
                ]),
                Line::from(""),
                Line::from("Enter saves · Esc cancels").dim(),
            ],
        ),
        Mode::ConfirmForget(id) => prompt(
            f,
            "Forget",
            vec![Line::from(format!(
                "Forget probe \"{id}\"? It comes back with the defaults if it checks in again. (y/n)"
            ))],
        ),
        Mode::Help => help(f),
        Mode::Normal => {}
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let [top, rule] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    let (dot, text) = match &app.connection {
        Ok(()) => (Span::raw("● ").fg(Color::Green), "live".to_string()),
        Err(why) => (Span::raw("● ").fg(Color::Red), why.clone()),
    };
    let conn = Line::from(vec![
        dot,
        Span::raw(text),
        Span::raw(format!("  {} ", app.client.base())).dim(),
    ]);
    let [name, tabs_area, conn_area] = Layout::horizontal([
        Constraint::Length(7),
        Constraint::Min(10),
        Constraint::Length(conn.width() as u16),
    ])
    .areas(top);
    f.render_widget(Paragraph::new(Span::raw(" mcast").bold()), name);
    // Probes needing attention, beside the tab name.
    let alerts = app
        .probes
        .iter()
        .filter(|p| matches!(p.health(), Health::Problem | Health::Offline))
        .count();
    let titles = Tab::ALL.iter().enumerate().map(|(i, t)| {
        let mut l = Line::from(format!("{} {}", i + 1, t.title()));
        if *t == Tab::Probes && alerts > 0 {
            l.push_span(Span::raw(format!(" {alerts}")).fg(Color::Red).bold());
        }
        l
    });
    let tabs = Tabs::new(titles)
        .select(app.tab.index())
        .highlight_style(Style::new().bold().fg(Color::Cyan));
    f.render_widget(tabs, tabs_area);
    f.render_widget(Paragraph::new(conn.right_aligned()), conn_area);
    f.render_widget(
        Block::new()
            .borders(Borders::TOP)
            .border_style(Style::new().dim()),
        rule,
    );
}

/// Hints for the current tab, or the latest status message.
fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let line = match app.status() {
        Some((text, level)) => {
            let color = match level {
                Level::Ok => Color::Green,
                Level::Error => Color::Red,
                Level::Info => Color::Cyan,
            };
            Line::from(Span::raw(format!(" {text}")).fg(color))
        }
        None => {
            let tab = match app.tab {
                Tab::Jobs => "s stop",
                Tab::Streams => "Enter listen · o output",
                Tab::Monitor => "",
                Tab::Media => "Enter page this · u upload · d delete",
                Tab::Probes => {
                    "u unicast · s SAP · l label · g groups · i interval · D defaults · f forget"
                }
            };
            let mut keys = vec!["p page", tab, "? help", "q quit"];
            keys.retain(|k| !k.is_empty());
            Line::from(format!(" {}", keys.join(" · "))).dim()
        }
    };
    f.render_widget(Paragraph::new(line), area);
}

/// A centred box of `width` x `height`, clipped to the screen.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [v] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [h] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(v);
    h
}

fn prompt(f: &mut Frame, title: &str, lines: Vec<Line>) {
    let area = centered(f.area(), 70, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .title(format!(" {title} "))
                .border_style(Style::new().fg(Color::Cyan)),
        ),
        area,
    );
}

fn help(f: &mut Frame) {
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(k, what)| {
            if what.is_empty() {
                Line::from(Span::raw(*k).add_modifier(Modifier::BOLD))
            } else {
                Line::from(vec![
                    Span::raw(format!("{k:>12}  ")).fg(Color::Cyan),
                    Span::raw(*what),
                ])
            }
        })
        .collect();
    prompt(f, "Keys (any key closes)", lines);
}

/// Style for the selected row.
pub fn highlight() -> Style {
    Style::new()
        .bg(Color::DarkGray)
        .add_modifier(Modifier::BOLD)
}

pub fn empty(f: &mut Frame, area: Rect, text: &str) {
    f.render_widget(
        Paragraph::new(Line::from(text).dim()).wrap(Wrap { trim: true }),
        area,
    );
}

pub fn state_color(state: &str) -> Color {
    match state {
        "running" => Color::Green,
        "failed" => Color::Red,
        "starting" | "stopping" => Color::Yellow,
        _ => Color::Gray,
    }
}
