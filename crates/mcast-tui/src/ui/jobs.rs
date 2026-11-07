use super::{empty, highlight, state_color};
use crate::app::App;
use ratatui::layout::Rect;
use ratatui::style::{Color, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let rows = app.job_rows();
    if rows.is_empty() {
        return empty(f, area, "Nothing playing. Press p to page.");
    }
    let items: Vec<ListItem> = rows
        .iter()
        .map(|j| {
            let mark = if j.kind == "send" { "▶ " } else { "♪ " };
            let mut head = vec![
                Span::raw(mark).fg(if j.kind == "send" {
                    Color::Cyan
                } else {
                    Color::Green
                }),
                Span::raw(j.title()).bold(),
                Span::raw("  "),
                Span::raw(j.state.clone()).fg(state_color(&j.state)),
            ];
            if app.conflict == Some(j.id) {
                head.push(Span::raw("  ← blocking your page (x stops it)").fg(Color::Red));
            }
            let mut lines = vec![Line::from(head)];
            if let Some(s) = &j.summary {
                lines.push(Line::from(format!("  {s}")).dim());
            }
            match &j.error {
                Some(e) => lines.push(Line::from(format!("  {e}")).fg(Color::Red)),
                None => lines.push(Line::from(format!("  {}", j.stats_line()))),
            }
            if let Some(n) = j.notes.last().filter(|_| j.active()) {
                lines.push(Line::from(format!("  {n}")).dim());
            }
            lines.push(Line::from(""));
            ListItem::new(lines)
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.selected()));
    f.render_stateful_widget(
        List::new(items).highlight_style(highlight()),
        area,
        &mut state,
    );
}
