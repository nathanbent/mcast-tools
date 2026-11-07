# mcast-web

The browser GUI for `mcast serve`. It only talks to the API under `/api/v1`;
the server knows nothing about it, and the `mcast` binary puts the two together.

No framework and no build step: `assets/` is served as-is (and embedded in
the binary by `build.rs`). Work on it with

```bash
mcast serve --web-dir crates/mcast-web/assets
```

and reload the page to see changes.

| File | |
|---|---|
| `index.html`, `app.css` | Layout and styles (light and dark) |
| `js/main.js` | Start-up: token prompt, initial load, event stream |
| `js/api.js` | The only module that calls the server |
| `js/store.js` | Lists from the API, kept current by server-sent events |
| `js/dom.js` | `h()` element builder and formatting helpers |
| `js/page.js` | The Page form |
| `js/jobs.js` | Now playing / recent jobs |
| `js/streams.js` | SAP-announced streams and Listen |
| `js/monitor.js` | Monitor table and level meters |
| `js/media.js` | Media list and uploads |
| `js/text.js` | Real-time text channels |
| `js/files.js` | Sending and receiving files |
| `js/probes.js` | The Probes view (`#probes`): alerts and alert rules, probes, verdicts, loss timeline, configuration |
| `js/phonebook.js` | Saved groups (names and descriptions), and suggestions for group fields |

Each panel is one module exporting `mount(element)`; add a panel by adding a
module and a `<section>` in `index.html`. There are two views, the console
(`#`) and the probes (`#probes`), switched by the links in the header.

Rules worth keeping:

- Build elements with `h()`, never `innerHTML`: stream names come from SAP
  announcements anyone on the network can send.
- Styles go in `app.css` or through `h(..., { style: {...} })`: the
  Content-Security-Policy forbids inline `style` attributes and scripts.
