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
| `js/theme.js`, `js/themes.js` | The display options (theme, accent color, density and text size, per browser): applied before the page draws, and the header's menu |
| `js/main.js` | Start-up: token prompt, initial load, event stream |
| `js/api.js` | The only module that calls the server |
| `js/store.js` | Lists from the API, kept current by server-sent events |
| `js/dom.js` | `h()` element builder and formatting helpers |
| `js/page.js` | The Page form |
| `js/jobs.js` | Now playing / recent jobs |
| `js/streams.js` | SAP-announced streams and Listen |
| `js/monitor.js` | Monitor table and level meters |
| `js/media.js` | Uploads |
| `js/phonebook.js` | Saved groups (names and descriptions), and suggestions for group fields |

Each panel is one module exporting `mount(element)`; add a panel by adding a
module and a `<section>` in `index.html`.

The header's display button sets the theme (the system's, light or dark),
an accent color, the density (comfortable or compact) and the text size,
remembered in that browser. A URL can set them for one screen without
storing them: `?theme=dark&accent=teal&density=compact&text=large`
(accents: `blue`, `teal`, `green`, `purple`, `orange`, `pink`; text:
`small`, `medium`, `large`). In `app.css` they are variables on `:root`:
colors, `--accent-l`/`--accent-d` (an accent's light and dark shade),
`--fs` (the text scale every font size is multiplied by) and the spacing
ones (`--pad`, `--gap`, `--ctl`, `--btn`, `--cell`), so new styles should
use them rather than fixed values.

Rules worth keeping:

- Build elements with `h()`, never `innerHTML`: stream names come from SAP
  announcements anyone on the network can send.
- Styles go in `app.css` or through `h(..., { style: {...} })`: the
  Content-Security-Policy forbids inline `style` attributes and scripts.
