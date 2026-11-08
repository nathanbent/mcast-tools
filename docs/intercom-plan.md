# Intercom control panel: overview

A control panel on top of `mcast serve`. You define **zones** (sets of
speakers), **announcements** (audio, chimes and where they go) and
**schedules**, then play announcements from big buttons, a schedule, another
system, or push to talk.

- Where things stand (handoff): `intercom-status.md`.
- What's planned next: `intercom-roadmap.md`.
- Design notes: `intercom-tokens.md` (access tokens and roles),
  `intercom-triggers.md` (the trigger API and webhooks).
- The user guide, config format and API: `crates/mcast-intercom/README.md`.

## Separable from the core

The panel is a **client of the mcast HTTP API**, as `mcast-tui` is. It
never links `mcast-control` or `mcast-server`, and has its own model types
for the few API shapes it reads. Moving it out means moving one crate and
pointing it at an `mcast serve` URL. It runs as **its own process**, with
its own port, keys and files. `mcast intercom ...` is a thin subcommand of
the `mcast` binary. On the Pi it's `mcast-intercom.service`
(`ENABLE_INTERCOM` in `pi/pi.conf`).

A few features needed the core to grow; each change is general, not
panel-specific:

- **Checked uploads and file details:** `PUT /media?audio=1`,
  `/media?audio=1`, `/media-info`.
- **Playlists:** the `files` source.
- **Push to talk:** the `live` source.
- **Delivery confirmation:** the monitor's `listeners` (receivers heard on
  RTCP), and send jobs' `ssrc`, which a listener's `about` names.

## The crate

```
crates/mcast-intercom/src/
  model.rs      the config (intercom.json) and its checks
  store.rs      reading and atomically writing the config (and owner-only files)
  backend.rs    trait Backend + HttpBackend (the mcast API, following its events)
  runner.rs     playing an announcement: a sender per group, conflicts, repeats, stop
  scheduler.rs  when schedules fire (time zones, days off, sites, grace for misses)
  tokens.rs     access tokens (tokens.json): roles, hashes, expiry, use counts
  webhooks.rs   outbound webhooks (webhooks.json): events, site filters, signing,
                delivery, and the delivery log (webhook-log.jsonl)
  history.rs    the play history and connection log (history/), stats and the monitor
  presence.rs   whether the speakers zones expect are reporting between plays
  inputs.rs     GPIO buttons and contacts (gpiocdev on Linux; a fake for tests)
  backup.rs     backups (intercom.json, or a .tar.gz with the sounds) and restoring
  cli.rs        mcast intercom list|check|upcoming|play|token|backup|restore
  server/       the panel's HTTP API (/api/intercom/v1), its page, events, TLS
assets/         the page: vanilla JS modules, no build step, strict CSP
```

Files beside `intercom.json`:
- **`tokens.json`, `webhooks.json` and `webhook-log.jsonl`:** owner-only.
- **`history/`:** a file per day, plus `connection.jsonl`.
- **`intercom.json.bak`:** the config before the last restore.
- **A self-signed certificate,** when the panel makes its own.

## Decisions that still hold

1. **A separate process,** talking to `mcast serve` over HTTP.
2. **Conflicts:**
   - Normal priority skips groups that are busy.
   - High and emergency take over from lower priorities.
   - Emergency takes over from high too.
3. **Schedules stay simple:** weekly (days and times) or once, with days off, groups, sites and pausing.
4. **No catch-up storm after an outage:** a schedule more than a minute late is recorded as missed, not played.
5. **The panel's config is the source of truth for the panel's things:** tags, categories, layout and the Panel's look live in `intercom.json`. Sound files and the phonebook stay on the mcast server.
