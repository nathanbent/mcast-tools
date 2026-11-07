use super::{empty, highlight};
use crate::app::App;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Row, Table, TableState};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let [top, rest] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(area);
    let output = if app.output.is_empty() {
        "default output"
    } else {
        &app.output
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("Listen plays on: "),
            Span::raw(output).fg(Color::Cyan),
            Span::raw("  (o to change) · Enter: listen, join text, receive files").dim(),
        ])),
        top,
    );
    if app.streams.is_empty() {
        return empty(
            f,
            rest,
            "No streams announced. Senders announce with SAP every few seconds.",
        );
    }
    let rows = app.streams.iter().map(|s| {
        let (group, format, playable) = match (&s.audio, &s.text, &s.file) {
            (Some(a), _, _) => (a.group.clone(), a.format(), a.playable),
            (None, Some(t), _) => (t.group.clone(), "real-time text".into(), true),
            (None, None, Some(fl)) => (fl.group.clone(), "files".into(), true),
            _ => ("–".into(), "unknown".into(), false),
        };
        let row = Row::new(vec![s.name.clone(), group, format, s.origin.clone()]);
        if playable {
            row
        } else {
            row.dim()
        }
    });
    let table = Table::new(
        rows,
        [
            Constraint::Fill(2),
            Constraint::Length(22),
            Constraint::Fill(1),
            Constraint::Length(16),
        ],
    )
    .header(
        Row::new(["Name", "Group", "Format", "From"])
            .bold()
            .fg(Color::Gray),
    )
    .row_highlight_style(highlight());
    let mut state = TableState::default().with_selected(Some(app.selected()));
    f.render_stateful_widget(table, rest, &mut state);
}
