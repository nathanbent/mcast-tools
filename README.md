# mcast-tools

A collection of multicast tools, written in Rust. The first set is audio:
uncompressed PCM over RTP multicast, using the same L16/L24 payloads as AES67.

## Install

### Prebuilt binaries

Each [release](https://github.com/nathanbent/mcast-tools/releases) has
binaries for Linux (x86_64 and ARM64, glibc 2.35 or newer, e.g. Ubuntu 22.04,
Debian 12, Raspberry Pi OS bookworm), macOS 11 or newer (Apple silicon and Intel) and
Windows (x86_64). Opus is built in; on Linux the only requirement is ALSA
(`libasound2`, already present on systems with sound).

```bash
mkdir -p ~/.local/bin && curl -fsSL https://github.com/nathanbent/mcast-tools/releases/latest/download/mcast-x86_64-unknown-linux-gnu.tar.gz | tar -xz -C ~/.local/bin
```

Use `mcast-aarch64-unknown-linux-gnu.tar.gz` on ARM64 Linux,
`mcast-aarch64-apple-darwin.tar.gz` or `mcast-x86_64-apple-darwin.tar.gz` on
macOS, and `mcast-x86_64-pc-windows-msvc.zip` on Windows. Any directory on your
`PATH` will do in place of `~/.local/bin`. Each release lists SHA-256 checksums
in `SHA256SUMS`.

### Build from source

On Debian/Ubuntu install the build dependencies first (ALSA for audio devices, libopus for Opus):

```bash
sudo apt-get install build-essential pkg-config libasound2-dev libopus-dev
```

Then install the `mcast` command into `~/.cargo/bin`:

```bash
cargo install --path crates/mcast
```

If the shell then says `Command 'mcast' not found`, `~/.cargo/bin` is not on your
`PATH` (rustup normally adds it; open a new shell, or run `source "$HOME/.cargo/env"`).

To build without installing, use `cargo build --release` and run the binary as
`./target/release/mcast`.

### Updating

`mcast` can update itself. A release binary downloads the latest release and
replaces itself in place. A binary built with cargo asks cargo to rebuild and
reinstall it from `main`, so it needs the build dependencies above.

```bash
mcast update --check              # is there anything newer?
mcast update                      # install it
mcast update --release            # switch a cargo build to the latest release
mcast update --source             # switch a release binary to building from main
mcast update --branch some-branch # build a branch (implies --source)
mcast update --path ~/mcast-tools # git pull a local clone, then install from it
```

`mcast --version` shows the commit the binary was built from. `--check` (and
`update` itself) compares it with what would be installed, through GitHub's
API or with `--path` in the clone, so a development build that is ahead of
`main` or of the latest release, or has commits they don't, isn't replaced
unless you add `--force`. Downloading needs `curl` and `tar` (both included in
macOS and Windows 10 and later), and permission to write to the binary's
directory. A cargo build is installed into `~/.cargo/bin`; if you run a copy
from somewhere else (such as `./target/release/mcast`), that copy is left as
it is.

### Making a release

Set the version in the root `Cargo.toml` (`[workspace.package] version`),
commit, and push a matching tag:

```bash
git tag v0.2.0 && git push origin v0.2.0
```

The [Release workflow](.github/workflows/release.yml) builds every platform,
checks the tag matches the version, and publishes the release with
`SHA256SUMS`. It also runs as a build-only check on pull requests that change
it, and can be started by hand from the Actions tab.

## Audio

Send a test tone, an audio file (WAV, MP3, FLAC, Ogg Vorbis, AAC/M4A, ALAC), or live input:

```bash
mcast audio send --tone 440                    # 440 Hz at -12 dBFS
mcast audio send --tone pad                    # soft, slowly changing chord (peak -12 dBFS)
mcast audio send --file music.mp3 --loop       # MP3, FLAC, Ogg, M4A/AAC, WAV...
mcast audio send --capture                     # default input device
mcast audio send --capture "USB" -g 239.69.0.5:5004 -e l24
```

`--tone pad` is easier to listen to than a sine: four chords (Cmaj7, Am7,
Fmaj7, G6) of 8 s each that cross-fade into one another, every note a slightly
detuned pair for a slow chorus and some stereo width. It's steady enough that a
dropped packet or click still stands out, and its upper notes sit at 290-530 Hz
so it sounds much the same on G.711 as on Opus or L16. `--level` sets its peak.

Senders announce their stream with SAP/SDP, so receivers find it on their own:

```bash
mcast audio send --name "Lobby" --tone 440     # announced as "Lobby"
mcast audio discover                           # list announced streams (+ added, - ended)
mcast audio recv                               # play the first stream announced
mcast audio recv --stream lobby                # play the stream whose name contains "lobby"
mcast audio recv --stream lobby --wav out.wav --duration 10
```

To skip discovery, give the group and format yourself:

```bash
mcast audio recv -g 239.69.0.1:5004 -r 48000 -c 2 -e l16 --latency 20 --device "Speakers"
```

Adjust the volume on either end in dB. Cuts are exact; boosts are limited so they
can't clip (a 5 ms look-ahead limiter when sending, a zero-latency soft knee
above -1 dBFS when receiving):

```bash
mcast audio send --file music.mp3 -v -6        # send 6 dB quieter
mcast audio recv --stream lobby -v 6           # play 6 dB louder
mcast audio recv --stream lobby -v -10 --wav quiet.wav
```

List devices with `mcast audio devices`.

### Codecs

| Codec | `-e` | Rate / channels | Default ptime, payload type |
|---|---|---|---|
| L16, L24 | `l16`, `l24` | any (48 kHz stereo by default) | 1 ms, 96 (10/11 for 44.1 kHz L16) |
| G.711 µ-law | `pcmu` / `g711u` | 8 kHz mono | 20 ms, 0 |
| G.711 A-law | `pcma` / `g711a` | 8 kHz mono | 20 ms, 8 |
| G.722 | `g722` | 16 kHz mono | 20 ms, 9 |
| Opus | `opus` | 48 kHz, mono (or `-c 2`) | 20 ms, 107 |

All of them can be sent and received. The G.711 and G.722 encoders and decoders
produce byte-for-byte the same output as ffmpeg's.

Sending converts the source to the codec's rate and channels (high-quality
windowed-sinc resampling, stereo averaged to mono), so any audio file or input
device works with any codec:

```bash
mcast audio send -e g711u --file page.mp3 -g 239.192.2.1:10000 --tos 184     # paging, DSCP EF
mcast audio send -e g722 --wav page.wav -g 239.192.2.1:10000
mcast audio send -e opus --opus-mode voice --bitrate 32 --opus-bandwidth wb --capture
```

Opus options: `--bitrate` (kbit/s, default 64), `--opus-mode voice|music`,
`--opus-bandwidth nb|wb|swb|fb`, `--no-fec` (in-band FEC is on by default, tuned for 5% loss).
`--ptime` and `--pt` override the defaults above.

Receiving: streams announced via SAP need no flags. With `--group`, G.711 and
G.722 are detected from the payload type; Opus needs `-e opus`:

```bash
mcast audio recv -g 239.192.2.1:10000               # G.711 or G.722, codec from payload type
mcast audio recv -g 239.192.2.1:10000 -e opus       # Opus
mcast audio recv -g 239.192.2.1:10000 --wav page.wav
```

Lost Opus packets are rebuilt from the next packet's FEC data, or concealed by libopus.

### Voice and music processing

`mcast audio send` can process audio before encoding, tuned for the content and
the codec (a port of the `mcast-play.sh` paging script):

```bash
mcast audio send --file announcement.mp3 -e g711u -m voice -L 500 -g 239.192.2.1:10000 --no-sap --tos 184
mcast audio send --wav song.wav -e g722 -m music -v -6
mcast audio send --wav song.wav -e g711u -m music -o preview.wav   # hear what the phone will decode
```

| Mode | Chain |
|---|---|
| `voice` (`-p`) | high-pass (250 Hz G.711 / 120 Hz wideband), noise reduction (10 dB, floor -40 dBFS), compressor 4:1 at -20 dB, de-esser, presence EQ (+3 dB at 2.5 kHz / +2 dB at 3 kHz), loudness -16 LUFS |
| `music` | high-pass (150 / 60 Hz), compressor 2:1 at -24 dB, on G.711 a mid EQ (-2 dB at 300 Hz, +2 dB at 1.8 kHz), loudness -18 LUFS |
| `none` (default) | nothing |

#### Chime and dead air

A page can open with a chime and keep the stream open a little on each side,
so phones don't clip the first or last word:

```bash
mcast audio send --file page.wav -e g711u -p -C -L 1000 -E 1000 -g 239.192.2.1:10000   # built-in chime
mcast audio send --file page.wav -e g711u -p -C tones/bell.wav -G 600 -o preview.wav   # your own tone
```

| Option | Meaning |
|---|---|
| `-C, --chime [FILE]` | Pre-tone: an audio file, or no value for the built-in three-note chime (C5-E5-G5, all within 500-1600 Hz so it sounds the same on G.711). `MCAST_CHIME=FILE` makes a file the default. |
| `-G, --chime-gap MS` | Silence between the chime and the audio (default 400) |
| `-L, --lead-in MS` | Silence before everything (default 0; try 800-1000) |
| `-E, --tail MS` | Silence after the audio (default 300) |

The chime is band-limited for the codec (200 Hz high-pass, the codec's
low-pass), normalised to -18 LUFS / -3 dBTP, and not voice-processed. `-v`
applies to it too.

With voice or music, a low-pass at the codec's band edge (3.4 kHz G.711,
7 kHz G.722 and wideband Opus) runs before loudness normalisation, and the
true peak is held at -2 dBTP. Opus tuning follows the mode.

| `mcast-play.sh` | `mcast audio send` |
|---|---|
| `-c g711u / g711a / g722 / opus` | `-e g711u / g711a / g722 / opus` |
| `FILE` (anything ffmpeg reads) | `--file FILE` (WAV, MP3, FLAC, Ogg, AAC/M4A, ALAC) |
| `-c opuswb` | `-e opus --opus-bandwidth wb --bitrate 32` |
| `-m voice` / `-p` / `-m music` | `-m voice` / `-p` / `-m music` |
| `-v DB`, `-L MS`, `-E MS`, `-s SECS`, `-t SECS` | same |
| `-C default` / `-C FILE`, `-G MS` | `-C` / `-C FILE`, `-G MS` (`DEFAULT_TONE` → `MCAST_CHIME`) |
| `-l` | `--loop` |
| `-g GROUP -P PORT` | `-g GROUP:PORT` |
| `-T PT` | `--pt PT` |
| `-o FILE` | `-o FILE` (`--render`) |
| (always `tos=184`, no SAP) | add `--tos 184 --no-sap` |

How it compares with the script (measured on the same speech and music
renders): timing, compressor and spectral balance match closely (the
compressor is a direct port and agrees with ffmpeg's to 0.01 dB; octave-band
balance is within about 1 dB). Two parts are re-implementations rather than
ports, and sound similar but not identical: noise reduction (calibrated
against ffmpeg's `afftdn`) and the de-esser. Loudness is normalised more
accurately: files are measured first (two passes, EBU R128), so output lands
near the target, where ffmpeg's single-pass `loudnorm` often falls short. Expect
`mcast` to be about 2 dB louder than the script for voice; use `-v` to taste.
Live input (`--capture`) uses a slow automatic gain control instead.

### The `mcast-play.sh` script

[`scripts/mcast-play.sh`](scripts/mcast-play.sh) is the ffmpeg-based original,
kept as a quick way to try ideas: a new filter or setting is a one-line change,
and `-o preview.wav` renders exactly what a phone would decode. Features that
prove themselves there get ported to `mcast audio send` (the table above maps
the options). It needs only bash and ffmpeg (with libopus for Opus):

```bash
scripts/mcast-play.sh page.wav -p -C default -L 1000 -E 1000      # G.711u to 239.192.2.1:10000
scripts/mcast-play.sh page.wav -p -C default -o preview.wav       # render instead of sending
scripts/mcast-play.sh -h                                          # all options
```

It has presets too, with the same commands as `mcast preset` and its own
options (`~/.config/mcast/play-presets/`, one file per preset; `$MCAST_PLAY_PRESETS`
overrides):

```bash
scripts/mcast-play.sh preset create page -p -C default -L 1000 -E 1000
scripts/mcast-play.sh preset set page -c g722                       # add or change options
scripts/mcast-play.sh @page page.wav                                # options given here win
scripts/mcast-play.sh @page page.wav -g 239.192.2.2 -o preview.wav
```

It first renders the finished page (chime, processing, dead air) to a
temporary WAV, then streams that with steady pacing: one 20 ms packet at a time,
released in real time by ffmpeg's `arealtime` (`-re` alone sends packets in
bursts of about seven every 140 ms, which phones' jitter buffers handle badly).
Packets are marked DSCP EF with `dscp=46`; ffmpeg's RTP output ignores `tos=184`.

To compare the two on the same input, render both and listen or measure:

```bash
scripts/mcast-play.sh page.wav -p -C default -L 500 -o script.wav
mcast audio send --file page.wav -e g711u -p -C -L 500 -o mcast.wav
```

### Presets

Save the options you use often under a name, then run them with `mcast @NAME`:

```bash
mcast preset create page-1 -g 239.192.2.1:10000 -e g711u --tos 184 --no-sap
mcast preset set page-1 -p -C -L 800 -E 1000    # add or change options
mcast @page-1 --file page.wav                   # run it with a file
mcast @page-1 --file page.wav -e g722 -v -3     # options typed here win
```

| Command | Does |
|---|---|
| `mcast preset create NAME [OPTIONS]` | New preset for `audio send` (`--recv`, `--discover`, `--monitor` or `--update`, before the options, for `audio recv`, `audio discover`, `monitor` or `update`) |
| `mcast preset set NAME OPTIONS` | Add or change options (creates a send preset if there isn't one) |
| `mcast preset unset NAME OPTION...` | Remove options, named `group`, `--group` or `-g` (`groups` for monitor's groups) |
| `mcast preset list [NAME]` (`ls`, `show`) | Show presets as the commands they run |
| `mcast preset copy FROM TO` (`cp`), `delete NAME` (`rm`) | |
| `mcast @NAME [OPTIONS]`, `mcast preset run NAME [OPTIONS]` | Run a preset |
| `mcast audio send -P NAME [OPTIONS]` (`--preset`) | The same, on the command itself (also `audio recv`, `audio discover`, `monitor`, `update`); repeatable, so presets can stack |

Options are the ones `mcast audio send` / `recv` / `discover` / `monitor` / `update` take, and are checked when
saved. A later option replaces the same option, or any it conflicts with:
setting `--file` drops a saved `--tone`, `-m music` drops `-p`. Stacked
presets apply left to right, and the command line comes last:

```bash
mcast preset set zone-2 -g 239.192.2.2:10000 -e g711u --tos 184 --no-sap
mcast preset set voice-page -p -C -L 800 -E 1000
mcast @zone-2 -P voice-page --file page.wav
mcast preset create lobby --recv -s lobby -v -6
mcast @lobby
```

A monitor preset saves the groups to watch along with its options. Groups
given on the command line replace the saved ones:

```bash
mcast preset create zones --monitor 239.192.2.1:10000 239.192.2.2:10000 --sap
mcast @zones                                    # watch both zones and SAP streams
mcast @zones --log -d 3600 > night.log
mcast @zones 239.192.2.3:10000                  # just this group (still with --sap)
```

A discover preset can save the SAP groups to listen on and whether to print SDP:

```bash
mcast preset create sap2 --discover --sap-group 239.195.255.255:9875 --sdp
mcast @sap2 -d 10
```

An update preset remembers where you install from:

```bash
mcast preset create dev --update -b dev --path ~/src/mcast-tools
mcast @dev --check                              # is there anything new?
mcast @dev -b main                              # this time, from main
```

Presets live in `~/.config/mcast/presets.conf` (`$XDG_CONFIG_HOME`,
`%APPDATA%` on Windows; `MCAST_PRESETS=FILE` overrides), one option per line,
and can be edited by hand:

```
[page-1]
audio send
--group=239.192.2.1:10000
--encoding=g711u
--voice
-L 800
```

### Discovery details

- Senders announce every 5 s (`--sap-interval`) on 239.255.255.255:9875, the
  group AES67/Dante devices use, and send a SAP deletion when they stop.
  `--no-sap` turns it off; `--sap-group` picks another group.
- Receivers listen on 239.255.255.255, 239.195.255.255 and 224.2.127.254
  (ffmpeg's default), so they also see AES67 devices and `ffmpeg -f sap` streams.
  Streams in formats we can't decode (e.g. G.729) are listed but skipped.
- `send --sdp stream.sdp` writes the SDP to a file, for players like
  `ffplay -protocol_whitelist file,udp,rtp stream.sdp` or VLC.
- ffmpeg can play our streams straight from SAP: `ffplay sap://239.255.255.255`.

Defaults: group `239.69.0.1:5004`, 48 kHz, 2 channels, L16, 1 ms packets, TTL 16.

Network options:

| Option | Meaning |
|---|---|
| `-g, --group` | Group and port, IPv4 or IPv6 (`[ff15::69]:5004`) |
| `-i, --iface` | IPv4 interface address, or interface index for IPv6 |
| `--ttl` | TTL / hop limit |
| `--no-loopback` | Don't deliver to receivers on the same host |

## Monitor

`mcast monitor` watches multicast streams and shows, per source, every second:

```bash
mcast monitor                                   # every stream announced via SAP
mcast monitor 239.192.2.1:10000 239.69.0.1:5004 # specific groups (any UDP traffic)
mcast monitor 239.192.2.1:10000 --sap           # both
mcast monitor 239.192.2.1:10000 -e opus         # meter a dynamic payload type with no SDP
mcast monitor --log -d 3600 > night.log         # one line per stream per interval
```

```
STREAM                         SOURCE                CODEC           PKT/S   KBIT/S   LOSS  LATE  JITTER  PTIME  TTL DSCP  LEVEL / STATUS
239.192.2.1:10000              10.0.0.5:49215        PCMU/8000/1      50.0     68.8   0.0%     0   0.4ms 20.0ms   63 EF    ██████████  -2.4 dBFS
239.69.0.42:5004               10.0.0.7:52098        opus/48000/1     45.0     59.6  10.0%     0   0.0ms 20.0ms   61 BE    ██████···· -21.7 dBFS
239.77.0.9:6000                10.0.0.9:58517        not RTP          10.0     24.0   0.0%     0       -      -    4 BE
239.69.0.50:5004               10.0.0.5:42332        G722/16000/1     50.0     68.8   0.0%     0   0.0ms 20.0ms   63 EF    silent
Lobby music 239.69.0.31:5004   10.0.0.5:37605        L16/48000/2    1000.0   1632.0   0.0%     0   0.0ms  1.0ms   16 BE    ███████··· -20.7 dBFS
```

- **LOSS / LATE**: from RTP sequence numbers (late = reordered or duplicated).
- **JITTER**: RFC 3550 interarrival jitter. High values mean bursty or
  unevenly delayed delivery, which phones' small jitter buffers handle badly.
- **TTL**: as received. The sender's TTL minus this is the number of router hops.
- **DSCP**: the QoS marking as it arrived (EF = 46, i.e. TOS 184). Shows
  whether the sender set it and whether the network kept it.
- **LEVEL**: decoded audio peak for L16/L24, G.711, G.722 and Opus, so a
  stream that's flowing but silent is obvious.
- Streams announced via SAP are joined and left automatically and named.
  Traffic that isn't RTP is still counted. A summary prints on exit.

TTL and DSCP are reported on Linux.

## Web GUI and API server

`mcast serve` runs everything above as a long-lived service, with a web GUI
at `/` and an HTTP/JSON API under `/api/v1` for home automation or scripts on
other machines:

```bash
mcast serve                                          # open http://127.0.0.1:8650/
MCAST_TOKEN=change-me mcast serve --listen 0.0.0.0:8650
```

```bash
curl -H "Authorization: Bearer change-me" -T page.mp3 http://pager:8650/api/v1/media/page.mp3
```

```bash
curl -H "Authorization: Bearer change-me" -H "Content-Type: application/json" \
  -d '{"group": "239.192.2.1:10000", "encoding": "g711u", "mode": "voice", "chime": "default",
       "lead_in_ms": 1000, "tos": 184, "source": {"type": "file", "path": "page.mp3"}}' \
  http://pager:8650/api/v1/senders
```

It keeps a list of SAP-announced streams, runs the stream monitor, and
streams job progress and changes as server-sent events (`GET /api/v1/events`).
A page to a group that one of its jobs is already sending to is refused with
`409 Conflict`. Audio files are uploaded to its media directory
(`~/.local/share/mcast/media` by default) and named by file name in requests.
Listening on anything but loopback requires a token. See
[docs/api.md](docs/api.md) for every route and field.

The GUI pages from uploaded files, live input, a test tone or the chord pad, shows what's
playing with a stop button, lists announced streams with a Listen button, and
shows the monitor with level meters. It asks for the token if one is set.
`--no-web` serves the API alone; `--web-dir crates/mcast-web/assets` serves
the GUI's files from disk, so edits show up on reload without rebuilding.

`mcast tui` does the same from a terminal, so it works over SSH:

```bash
ssh pager mcast tui                                  # the server on that box
MCAST_TOKEN=change-me mcast tui --url http://pager:8650
```

Tabs for what's playing, announced streams, the monitor and media; `p` opens
the page form, `?` lists the keys. Like the web GUI, it's a client of the
API: it needs `mcast serve` running, and every client sees the same jobs.

## Layout

| Crate | Purpose |
|---|---|
| `crates/mcast-core` | Multicast sockets (v4/v6), RTP, SAP announce/directory, SDP parse/write |
| `crates/mcast-audio` | Codecs (L16/L24, G.711, G.722, Opus), voice/music processing (`process`, `dsp`), resampler, packetizer, jitter buffer, sender & receiver, device I/O (cpal) |
| `crates/mcast-monitor` | Stream monitor: per-source rate, loss, jitter, TTL, DSCP, level |
| `crates/mcast-control` | Send/receive requests shared by the CLI and the API; jobs, SAP list and monitor as background services |
| `crates/mcast-server` | HTTP/JSON API (`mcast serve`) |
| `crates/mcast-tui` | Terminal front end (`mcast tui`), an API client built on ratatui |
| `crates/mcast-web` | Web GUI: plain HTML/CSS/JS modules in `assets/` (one per panel, no build step), embedded in the binary |
| `crates/mcast` | The `mcast` CLI |
| `scripts/mcast-play.sh` | ffmpeg-based paging script |

New tool families go in their own crate (e.g. `crates/mcast-<thing>`) and get a
subcommand in the CLI.

## Roadmap ideas

- Audio: PTP-aligned timestamps (AES67), resampling on playback
- Other: file transfer, chat, ping/latency, relay/bridge between interfaces
