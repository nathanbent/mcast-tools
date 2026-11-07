use super::{empty, highlight};
use crate::app::App;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Stylize};
use ratatui::widgets::{Row, Table, TableState};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    if app.phonebook.is_empty() {
        return empty(
            f,
            area,
            "No saved groups yet. Press a to add one; then use its name wherever a group goes \
             (←→ on a form's Group field picks one).",
        );
    }
    let rows = app
        .phonebook
        .iter()
        .map(|e| Row::new(vec![e.name.clone(), e.group.clone(), e.description.clone()]));
    let table = Table::new(
        rows,
        [
            Constraint::Fill(1),
            Constraint::Length(24),
            Constraint::Fill(2),
        ],
    )
    .header(
        Row::new(["Name", "Group", "Description"])
            .bold()
            .fg(Color::Gray),
    )
    .row_highlight_style(highlight());
    let mut state = TableState::default().with_selected(Some(app.selected()));
    f.render_stateful_widget(table, area, &mut state);
}
