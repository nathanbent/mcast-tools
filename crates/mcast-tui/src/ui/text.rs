//! The Text tab: a channel's conversation, and the line being typed.

use super::empty;
use crate::app::{App, Mode};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

/// HH:MM from Unix milliseconds (UTC; the server's clock).
fn clock(ms: u64) -> String {
    let mins = ms / 60_000;
    format!("{:02}:{:02}", mins / 60 % 24, mins % 60)
}

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let Some(job) = app.current_channel() else {
        return empty(
            f,
            area,
            "No text channel open. Press j to join one, or Enter on a text stream in Streams.",
        );
    };
    let open = app.channels().len();
    let composing = matches!(app.mode, Mode::Compose(_));
    let [head, log, input] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(if job.text_sender() { 2 } else { 0 }),
    ])
    .areas(area);

    let mut title = vec![
        Span::raw(job.name.clone().unwrap_or_else(|| "Text".into())).bold(),
        Span::raw(format!("  {}", job.summary.clone().unwrap_or_default())).dim(),
    ];
    if open > 1 {
        title.push(Span::raw(format!("  ({open} channels: [ ] to switch)")).fg(Color::Cyan));
    }
    f.render_widget(Paragraph::new(Line::from(title)), head);

    let lines = app.texts.get(&job.id).map(Vec::as_slice).unwrap_or(&[]);
    if lines.is_empty() {
        empty(f, log, "Nothing said yet.");
    } else {
        let text: Vec<Line> = lines
            .iter()
            .map(|l| {
                let who = Span::raw(format!("{}: ", l.from)).bold();
                Line::from(vec![
                    Span::raw(format!("{} ", clock(l.at_ms))).dim(),
                    if l.own { who.fg(Color::Cyan) } else { who },
                    Span::raw(l.text.clone()),
                ])
            })
            .collect();
        // Keep the newest lines in view (roughly: wrapped lines take more room).
        let height = log.height as usize;
        let skip = text.len().saturating_sub(height);
        let para = Paragraph::new(text[skip..].to_vec()).wrap(Wrap { trim: false });
        f.render_widget(para, log);
    }

    if job.text_sender() {
        let line = match &app.mode {
            Mode::Compose(t) => Line::from(vec![
                Span::raw("> ").fg(Color::Cyan),
                Span::raw(t.clone()),
                Span::raw("▏").fg(Color::Cyan),
                Span::raw("   Enter sends · Esc stops typing").dim(),
            ]),
            _ => Line::from("i or Enter to type a message").dim(),
        };
        let block = Block::new()
            .borders(Borders::TOP)
            .border_style(if composing {
                ratatui::style::Style::new().fg(Color::Cyan)
            } else {
                ratatui::style::Style::new().dim()
            });
        f.render_widget(Paragraph::new(line).block(block), input);
    }
}
