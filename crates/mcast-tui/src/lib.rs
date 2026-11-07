//! Terminal front end for `mcast serve`. Like the web GUI it only talks to
//! the API, so it can run on the server (e.g. over SSH) or anywhere that can
//! reach it, and it sees the same jobs as every other client.

mod api;
mod app;
mod form;
mod keys;
mod model;
mod ui;

use anyhow::{bail, Result};
use app::{App, Msg};
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use std::sync::mpsc;
use std::time::Duration;

/// Run until the user quits. `url` is the server, e.g. "http://127.0.0.1:8650".
pub fn run(url: &str, token: Option<String>) -> Result<()> {
    let client = api::Client::new(url, token);
    // Fail before taking over the terminal.
    if let Err(e) = client.get::<serde_json::Value>("/health") {
        match e.status {
            401 => bail!("{url} wants a token: pass --token or set MCAST_TOKEN"),
            0 => bail!("{}; is `mcast serve` running?", e.message),
            _ => bail!("{url}: {}", e.message),
        }
    }
    let (tx, rx) = mpsc::channel();
    let mut app = App::new(client.clone(), tx.clone());
    app.refresh_all();
    client.follow(move |feed| tx.send(Msg::Feed(feed)).is_ok());

    // Restores the terminal on panic too.
    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        while !app.quit {
            terminal.draw(|f| ui::draw(f, &app))?;
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Press {
                        keys::handle(&mut app, k);
                    }
                }
            }
            while let Ok(msg) = rx.try_recv() {
                app.handle(msg);
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests;
