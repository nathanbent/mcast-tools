use super::{empty, highlight};
use crate::app::App;
use crate::model::bytes;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Stylize};
use ratatui::widgets::{Row, Table, TableState};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    if app.media.is_empty() {
        return empty(
            f,
            area,
            "No audio on the server yet. Press u to upload a local file.",
        );
    }
    let rows = app
        .media
        .iter()
        .map(|m| Row::new(vec![m.name.clone(), bytes(m.bytes)]));
    let table = Table::new(rows, [Constraint::Fill(1), Constraint::Length(10)])
        .header(Row::new(["File", "Size"]).bold().fg(Color::Gray))
        .row_highlight_style(highlight());
    let mut state = TableState::default().with_selected(Some(app.selected()));
    f.render_stateful_widget(table, area, &mut state);
}
