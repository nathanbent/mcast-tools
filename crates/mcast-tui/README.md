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
| `app.rs` | State, and how API results and events change it |
| `actions.rs` | Every API call, each in a background thread that reports back as a `Msg` |
| `form/` | Forms (no drawing): the generic `Form` in `mod.rs`, then `page.rs`, `text.rs` (join a channel), `files.rs` (send, receive) |
| `keys.rs` | Key bindings, per mode and per tab, and the help text |
| `ui/` | Drawing: `mod.rs` (header, footer, overlays), `form.rs`, and one file per tab |

Tabs: Now playing, Text (a channel's conversation; `i` types, `j` joins,
`[ ]` switch), Streams (Enter listens to audio, joins text, receives
files), Monitor, Media (Enter pages audio, `f` sends a file, `r` receives,
`u` uploads). `?` lists every key.

API calls never run on the UI thread: actions spawn a thread that sends a
`Msg` back, so a slow page start (loudness measurement) doesn't freeze the screen.

`cargo test -p mcast-tui` runs the TUI against a real in-process server and
checks what it draws using ratatui's test backend.
