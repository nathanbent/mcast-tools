//! The open form, drawn over the current tab.

use super::centered;
use crate::app::App;
use crate::form::{FormKind, Kind};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &App, kind: FormKind) {
    let form = app.forms.get(kind);
    let visible = form.visible();
    let selected = form.selected.min(visible.len() - 1);
    let mut lines: Vec<Line> = visible
        .iter()
        .enumerate()
        .map(|(row, &i)| {
            let field = &form.fields[i];
            let on = row == selected;
            let mut value = field.display();
            match &field.kind {
                Kind::Choice(_) if on => value = format!("◀ {value} ▶"),
                Kind::Text | Kind::Number if on => value.push('▏'),
                _ => {}
            }
            // A phonebook name in a Group field: show where it goes.
            let note = (field.key == "group" && kind != FormKind::Book)
                .then(|| app.book_entry(&field.value))
                .flatten()
                .map(|e| format!("  → {}", e.group));
            let label = Span::raw(format!("{:>20}  ", field.label));
            let value = Span::raw(value);
            let mut line = if on {
                Line::from(vec![label.fg(Color::Cyan).bold(), value.bold()])
            } else {
                Line::from(vec![label.dim(), value])
            };
            if let Some(n) = note {
                line.push_span(Span::raw(n).fg(Color::Green));
            }
            line
        })
        .collect();
    lines.push(Line::from(""));
    let on_group = form
        .fields
        .get(visible[selected])
        .is_some_and(|f| f.key == "group" && f.kind == Kind::Text);
    let arrows = if on_group && !form.book.is_empty() {
        "←→ phonebook"
    } else {
        "←→ choices"
    };
    lines.push(if app.busy {
        Line::from("Starting…").fg(Color::Yellow)
    } else {
        Line::from(format!(
            "Enter {} · ↑↓ fields · {arrows} · Esc closes",
            kind.submit()
        ))
        .dim()
    });
    let area = centered(f.area(), 64, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(format!(" {} ", kind.title()))
                .border_style(Style::new().fg(Color::Cyan)),
        ),
        area,
    );
}
