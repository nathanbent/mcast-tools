use super::{empty, highlight};
use crate::app::App;
use crate::model::MonitorRow;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Row, Table, TableState};
use ratatui::Frame;

const FLOOR: f64 = -60.0;
const METER: usize = 20;

/// RMS as a bar, peak as a tick.
fn meter(r: &MonitorRow) -> Line<'static> {
    let pos = |db: Option<f64>| {
        db.map_or(0, |d| {
            (((d - FLOOR) / -FLOOR).clamp(0.0, 1.0) * METER as f64).round() as usize
        })
    };
    let (rms, peak) = (pos(r.rms_dbfs), pos(r.peak_dbfs));
    let hot = r.peak_dbfs.is_some_and(|p| p > -1.0);
    let mut spans = Vec::new();
    for i in 0..METER {
        spans.push(if r.peak_dbfs.is_some() && i + 1 == peak.max(1) {
            Span::raw("│").fg(if hot { Color::Red } else { Color::White })
        } else if i < rms {
            Span::raw("█").fg(Color::Green)
        } else {
            Span::raw("·").fg(Color::DarkGray)
        });
    }
    spans.push(Span::raw(match r.peak_dbfs {
        Some(p) => format!(" {p:>5.1}"),
        None => "  silent".into(),
    }));
    Line::from(spans)
}

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let snapshot = match &app.monitor {
        None => return empty(f, area, "Loading…"),
        Some(None) => {
            return empty(
                f,
                area,
                "The monitor is off (server started with --no-monitor).",
            )
        }
        Some(Some(s)) if s.streams.is_empty() => {
            return empty(f, area, "No traffic on watched groups.")
        }
        Some(Some(s)) => s,
    };
    let rows = snapshot.streams.iter().map(|r| {
        let idle = r.idle_ms > 2000.0;
        let rate = if idle {
            format!("idle {:.0} s", r.idle_ms / 1000.0)
        } else {
            format!("{:.0} kbit/s", r.kbit_per_sec)
        };
        let loss = Cell::from(format!("{:.1} %", r.loss_percent)).style(if r.loss_percent > 1.0 {
            Style::new().fg(Color::Red)
        } else {
            Style::new()
        });
        let row = Row::new(vec![
            Cell::from(r.name.clone().unwrap_or_else(|| r.source.clone())),
            Cell::from(r.group.clone()),
            Cell::from(r.codec.clone().unwrap_or_else(|| "?".into())),
            Cell::from(rate),
            loss,
            Cell::from(r.jitter_ms.map_or("–".into(), |j| format!("{j:.1} ms"))),
            Cell::from(meter(r)),
        ]);
        if idle {
            row.dim()
        } else {
            row
        }
    });
    let widths = [
        Constraint::Min(16),
        Constraint::Length(22),
        Constraint::Length(13),
        Constraint::Length(12),
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(METER as u16 + 8),
    ];
    let table = Table::new(rows, widths)
        .header(
            Row::new([
                "Stream",
                "Group",
                "Codec",
                "Rate",
                "Loss",
                "Jitter",
                "Level (dBFS peak)",
            ])
            .bold()
            .fg(Color::Gray),
        )
        .row_highlight_style(highlight());
    let mut state = TableState::default().with_selected(Some(app.selected()));
    f.render_stateful_widget(table, area, &mut state);
}
