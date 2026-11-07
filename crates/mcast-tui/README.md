# mcast-tui

The terminal front end: `mcast tui [--url http://127.0.0.1:8650] [--token T]`.
It only talks to the API (like the web GUI), so it needs `mcast serve`
running, works over SSH or from another machine, and sees the same jobs and
overlap rules as every other client.

| File | |
|---|---|
| `lib.rs` | `run()`: checks the server, then the draw / key / message loop |
| `api.rs` | The only module that calls the server; follows server-sent events in a thread |
| `model.rs` | What the API returns, as far as the TUI needs it (independent of the server's types) |
| `app.rs` | State, and how API results and events change it; actions run in background threads |
| `form.rs` | The page form's fields and the request they build (no drawing) |
| `keys.rs` | Key bindings, per mode, and the help text |
| `ui/` | Drawing: `mod.rs` (header, footer, overlays) and one file per tab |

API calls never run on the UI thread: actions spawn a thread that sends a
`Msg` back, so a slow page start (loudness measurement) doesn't freeze the screen.

`cargo test -p mcast-tui` runs the TUI against a real in-process server and
checks what it draws using ratatui's test backend.
