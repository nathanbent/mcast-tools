# Design: page metadata over multicast

Status: built (mcast-tools after 0.5.12).

## The gap

A text page reaches a display two ways. Through the panel's API it comes
whole: its lines, the announcement's name and priority, and when it ends.
Over multicast (real-time text, RFC 4103) it is only words, a line at a
time. A display that hears a page over multicast alone therefore has to
guess:

- where the page starts and ends (lines that arrive close together);
- whether it is an emergency (a list of words to look for);
- how long to keep it up (a fixed time), and it can't be taken down early;
- what to head it with (the sender's name, always "Intercom");
- whether it is the same page it also got from the panel (the same words
  within 30 seconds).

Multicast is the route that still works when the panel's API can't be
reached from a display, so it should carry enough to show a page properly
by itself.

## Where the metadata goes

In the text stream itself, in T.140's own extension point. T.140 reserves
SOS (U+0098) … ST (U+009C) as a "general protocol element introducer": a
string of at most 256 bytes between them that a receiver doesn't display
and ignores if it doesn't know it. (This is from memory of T.140; check
the wording in the recommendation before building.)

Why there, and not beside the stream:

| | In the text (SOS … ST) | RTCP APP packets | A second stream |
|---|---|---|---|
| Arrives in order with the lines it describes | yes | no (another port, another schedule) | no |
| Covered by the text's redundancy (two lost packets in a row) | yes | no | needs its own |
| New sockets, ports, firewall rules | none | none | yes |
| A plain T.140 receiver (a phone) | ignores it, by the standard | never sees it | never sees it |

The cost is a receiver that doesn't follow the standard and shows the
string. Our own do the right thing already: `mcast_plain::clean` drops
control strings and their contents, so `mcast text recv` from this version
on shows such a page as its plain lines. A zone can be set to send its
displays the words alone (`plain_displays`).

## What is sent

Each element is `SOS`, the text below, `ST`. Fields are separated by
spaces; `name` is last and takes the rest, so it needs no quoting.

| Element | Meaning |
|---|---|
| `mc1 page ID PRIORITY HOLD name…` | A page begins. Lines follow as ordinary text |
| `mc1 show ID` | The page is complete: show it now |
| `mc1 clear ID [now]` | The page's run is over: take it down after its `HOLD`, or at once with `now` (stopped by hand) |

- `mc1`: this format, version 1. A receiver ignores any other tag.
- `ID`: the run and the time the page went up, `RUN.AT_MS`, the same pair
  the panel's API gives a page. A display that gets a page both ways
  matches them by this, not by their words.
- `PRIORITY`: `n`, `h` or `e` (normal, high, emergency).
- `HOLD`: seconds to stay up after `clear` (10 today).
- `name`: the announcement's name, cut to fit the 256 bytes.

A page on the wire, with U+2028 between lines as now:

```
␘mc1 page 41.1794764572215 e 10 Fire drill␜⚠️ Fire drill 🔥 at 3pm
Use the east stairs ➡️␘mc1 show 41.1794764572215␜
```

and when the run ends, `␘mc1 clear 41.1794764572215␜` with no text. An
element never sits on a line of its own that ends, so a receiver that
only drops it shows no blank line for it.

## What a receiver does

- Lines between `page` and `show` are one page with that name and
  priority. It goes up at `show`, and stays until `clear` plus `HOLD`.
- No `clear` within a safety limit (10 minutes): it comes down anyway. A
  display that joined late, or lost three packets in a row, is never
  stuck with a page.
- `page` lost: the lines arrive as plain text and are shown as they are
  today, gathered by timing. Nothing is worse than now.
- Text outside a page (someone using `mcast text send`) is unchanged.

## Where it is in the code

- **mcast-text**
  - `page`: the elements, written and read.
  - `lines`: the line buffer takes control strings out of the text as
    they come and hands on those that are elements.
  - `transcript`: puts a page together, as `Entry::Page` (its lines with
    what it is) and `Entry::Clear`. A page whose `show` is lost goes up
    once its sender has been quiet for 3 s.
  - `sender`: `push_page` and `push_element`. Elements share the 400-byte
    chunking and the redundancy with the text.
- **mcast (CLI)**
  - `text recv --json` prints `{"type": "page", "id", "name", "priority",
    "hold_s", "text": [lines], …}` and `{"type": "clear", "id", "now"}`.
    Without `--json` a page is printed as its lines.
  - `text send --page TITLE [--priority P] [--hold SECS] MESSAGE` sends
    one, and its `clear` straight after, so it's up for `--hold` seconds.
- **mcast serve API**
  - `POST /jobs/{id}/text` takes an optional `page` (`id`, `name`,
    `priority`, `hold_s`); `POST /jobs/{id}/text/clear` (`page`, `now`)
    ends one.
  - Lines read back from a channel have `page` when they're of one, and a
    `text_cleared` event says when one comes down.
- **Intercom panel**: sends each page to its zones' display groups as a
  page, and `clear` when the run ends (`now` when it was stopped). A zone
  with `plain_displays` gets the words alone, as does an mcast server from
  before pages.
- **TouchFrame**: reads `page` and `clear` from `--json`. For those pages
  nothing is guessed: no timing, no word list, no fixed hold; and a page
  that also comes from the panel is matched to it by its id.

## Not solved here

Anyone who can send to the group can put up an emergency page. That is
already so (the word list), and this makes it explicit rather than worse.
A signed element (`mc1 sig …`, an HMAC over the page with a key the panel
and its displays share) fits the same scheme and can be added later
without changing the rest.
