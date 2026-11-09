# mcast-tools

A collection of multicast tools, written in Rust: audio over RTP multicast
(the AES67 PCM payloads, plus G.711, G.722 and Opus), real-time text, files
and pictures, and a stream monitor.

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

You also need Rust 1.89 or newer. The `cargo` and `rustc` packages in Debian
and Ubuntu are too old (Ubuntu 24.04 has 1.75, which fails with `feature
edition2024 is required`), so install Rust with [rustup](https://rustup.rs)
instead:

```bash
sudo apt-get remove cargo rustc
```

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

```bash
source "$HOME/.cargo/env"
```

Then install the `mcast` command into `~/.cargo/bin`:

```bash
cargo install --locked --path crates/mcast
```

`--locked` builds with the dependency versions in `Cargo.lock`, the ones CI
tests (and what `mcast update` uses).

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

Set the version in the root `Cargo.toml` (`[workspace.package] version`) and
merge it to `main`. Then either push a matching tag:

```bash
git tag v0.2.0 && git push origin v0.2.0
```

or, without a local clone, open the [Release workflow](.github/workflows/release.yml)
in the Actions tab, choose **Run workflow** on `main`, and enter the tag
(`v0.2.0`); the workflow creates the tag on the commit it built. From the
command line, the same is:

```bash
gh workflow run release.yml --ref main -f release=v0.2.0
```

Either way the workflow checks the tag matches the version (and, for a manual
run, that it's on `main` and the tag isn't already used for another commit),
builds every platform, and publishes the release with `SHA256SUMS` only if
all of them built. Run it with no tag for a build-only check; it also runs
that way on pull requests that change it.

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

A page can open with a chime, close with another, and keep the stream open a
little on each side, so phones don't clip the first or last word:

```bash
mcast audio send --file page.wav -e g711u -p -C -L 1000 -E 1000 -g 239.192.2.1:10000   # default chime
mcast audio send --file page.wav -e g711u -p -C ding-dong --end-chime -g 239.192.2.1:10000   # ding-dong in, falling out
mcast audio send --file page.wav -e g711u -p -C tones/bell.wav -G 600 -o preview.wav   # your own tone
mcast audio send --chime-only -C alert -e g711u -g 239.192.2.1:10000                     # just the chime
```

| Option | Meaning |
|---|---|
| `-C, --chime [NAME\|FILE]` | Chime before the audio: a built-in (below), an audio file, or no value for the default (`rising`). `MCAST_CHIME=FILE` makes a file the default. |
| `-G, --chime-gap MS` | Silence between the chime and the audio (default 400) |
| `--end-chime [NAME\|FILE]` | Chime after the audio, before the tail, to mark the end of the page; no value for the default (`falling`, or `MCAST_END_CHIME=FILE`). A live `--capture` page gets it when you stop it; a file or tone only when it ends on its own. |
| `--end-chime-gap MS` | Silence between the audio and the end chime (default 400) |
| `--chime-only` | Send no audio, just the chime(s): `-C` (the default if neither is given) and/or `--end-chime` |
| `-L, --lead-in MS` | Silence before everything (default 0; try 800-1000) |
| `-E, --tail MS` | Silence after the audio (default 300) |

Built-in chimes, all with fundamentals within 500-1600 Hz so they sound the
same on G.711 as on wideband codecs (built-in names win over files; use
`./bell` for a file called `bell`). `mcast audio chimes` lists them, and
`mcast audio chimes save NAME FILE.wav` saves one to listen to:

| Name | Sound |
|---|---|
| **Chimes** | |
| `rising` (default) | Three rising notes, C5-E5-G5: the classic attention chime |
| `falling` (default end chime) | The same notes falling, G5-E5-C5: the classic end-of-page chime |
| `ding-dong` | Two notes, E5 then C5, like a doorbell |
| `arpeggio` | Four rising notes, C5-E5-G5-C6 |
| `arpeggio-down` | Four falling notes, C6-G5-E5-C5 |
| `bing-bong` | Two slow notes, A5 then F5, as at a station or airport |
| `ding-dang-dong` | Three slow falling notes, C6-A5-F5 |
| `westminster` | The Westminster quarter phrase, B5-A5-G5-D5, as from a clock tower |
| `triad` | A C-major chord, struck all at once |
| `gentle` | Three soft, pure notes, D5-F#5-A5, slowly |
| `welcome` | Four bright rising notes, E5-G5-C6-E6 |
| `farewell` | Four bright falling notes, E6-C6-G5-E5 |
| `pentatonic` | A quick rising run, C5 to C6 |
| `question` | Two notes rising a fifth, C5-G5 |
| `fanfare` | A short brassy fanfare, G5-G5-G5-C6 |
| **Bells** | |
| `bell` | A single struck bell (A5) with a long ring |
| `church-bell` | A deep church bell, D5, with its hum and a long ring |
| `tubular-bell` | An orchestral tubular bell, E5 |
| `school-bell` | An electric school bell ringing for two seconds |
| `telephone` | An old telephone bell: two rings |
| `ships-bell` | A ship's bell struck in two pairs |
| `handbell` | A handbell, shaken a few times |
| `bicycle-bell` | A bicycle bell, rung twice |
| `triangle` | An orchestral triangle with a shimmering ring |
| `glockenspiel` | Three glockenspiel notes, C6-E6-G6 |
| `marimba` | Four mellow marimba notes, C5-E5-G5-C6 |
| `music-box` | A tinkling music-box phrase |
| `singing-bowl` | A singing bowl: a soft, slowly beating ring |
| `wind-chimes` | Wind chimes, a scatter of high notes |
| `cowbell` | A cowbell, struck twice |
| **Beeps and tones** | |
| `beep` | A single 1 kHz beep |
| `double-beep` | Two short 1 kHz beeps |
| `triple-beep` | Three short 1 kHz beeps |
| `pips` | Time-signal pips: five short and one long |
| `blip` | A quick rising blip |
| `notify` | Two short high notes, like a message notification |
| `error` | Two low buzzy notes falling: 'no' |
| `sonar` | A sonar ping with an echo |
| `countdown` | Three short beeps then a long high one, like a race start |
| `steady` | A steady 1 kHz tone for two seconds |
| **Alerts and sirens** | |
| `alert` | Four quick alternating notes (B5/G5) for urgent pages |
| `siren-wail` | A siren slowly wailing up and down |
| `siren-yelp` | A siren yelping quickly up and down |
| `hi-lo` | Alternating high and low tones |
| `klaxon` | A rasping horn, 'a-oo-ga', twice |
| `buzzer` | A harsh buzzer |
| `alarm` | Fast beeping, like an alarm clock |
| `whoop` | A rising whoop, twice: an evacuation tone |
| `warble` | A fast-warbling alarm tone |

Chimes are band-limited for the codec (200 Hz high-pass, the codec's
low-pass), normalised to -18 LUFS / -3 dBTP, and not voice-processed. `-v`
applies to them too. The web GUI and `mcast tui` pick both chimes from a list,
grouped by kind, and the web Page form has a **Chime** button that sends just
the chime(s) to the group, and a ▶ button by each list to hear the chime in
the browser.

That's a long list, so hide the ones you don't want: in the web GUI with
**Choose which chimes to list…** under the chime lists (or in the intercom
panel's announcement form), or from the command line:

```bash
mcast audio chimes                        # what the lists offer
mcast audio chimes hide klaxon cowbell    # leave these out
mcast audio chimes list --all             # everything, marked (hidden)
mcast audio chimes show --all             # offer them all again
```

Hiding only trims the lists in the web GUI, `mcast tui` and the intercom
panel: `-C NAME`, the API and announcements that already use a hidden chime
still play it. It's kept in `chimes.json` in the mcast config directory (or
`$MCAST_CHIMES`), so every client of a server shares it.

With voice or music, a low-pass at the codec's band edge (3.4 kHz G.711,
7 kHz G.722 and wideband Opus) runs before loudness normalisation, and the
true peak is held at -2 dBTP. Opus tuning follows the mode.

| `mcast-play.sh` | `mcast audio send` |
|---|---|
| `-c g711u / g711a / g722 / opus` | `-e g711u / g711a / g722 / opus` |
| `FILE` (anything ffmpeg reads) | `--file FILE` (WAV, MP3, FLAC, Ogg, AAC/M4A, ALAC) |
| `pad` in place of the file (64 s, `-t SECS`, or `-l`) | `--tone pad` |
| `-c opuswb` | `-e opus --opus-bandwidth wb --bitrate 32` |
| `-m voice` / `-p` / `-m music` | `-m voice` / `-p` / `-m music` |
| `-v DB`, `-L MS`, `-E MS`, `-s SECS`, `-t SECS` | same |
| `-C default` / `-C FILE`, `-G MS` | `-C` / `-C FILE`, `-G MS` (`DEFAULT_TONE` → `MCAST_CHIME`); also `-C NAME`, `--end-chime`, `--chime-only` |
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
scripts/mcast-play.sh pad -l                                      # test pad (same as --tone pad), looped
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

### Phonebook

Save the groups you use under a name and a description, then use the name
wherever a group goes:

```bash
mcast book add zone-1 239.192.2.1:10000 -d "Ground floor paging speakers (G.711)"
mcast book add "Lobby music" 239.69.0.31:5004 -d "Background music"
mcast book                                     # list them
mcast audio send -g zone-1 -e g711u -p --file page.wav
mcast audio recv -g "lobby music"              # names match without regard to case
mcast monitor zone-1 "Lobby music"             # named in the STREAM column
```

```
NAME         GROUP              DESCRIPTION
Lobby music  239.69.0.31:5004   Background music
zone-1       239.192.2.1:10000  Ground floor paging speakers (G.711)
```

| Command | Does |
|---|---|
| `mcast book [list [TEXT]]` (`ls`) | List entries, or those whose name, group or description contains TEXT |
| `mcast book add NAME GROUP [-d DESCRIPTION]` | Add an entry (`--force` replaces one of the same name) |
| `mcast book edit NAME [-g GROUP] [-d DESCRIPTION] [--rename NEW]` | Change one |
| `mcast book remove NAME` (`rm`) | Remove one |

Names are letters, digits, spaces, `-`, `_` and `.`. Presets can use them too
(`mcast preset create page --group zone-1 ...`); the name is looked up each
time the preset runs, so moving `zone-1` to a new group moves the preset with
it. The phonebook lives in `~/.config/mcast/phonebook.conf` (`$XDG_CONFIG_HOME`,
`%APPDATA%` on Windows; `MCAST_PHONEBOOK=FILE` overrides) and can be edited by
hand:

```
[zone-1]
group = 239.192.2.1:10000
description = Ground floor paging speakers (G.711)
```

`mcast serve` shares it: the web GUI has a Phonebook panel for adding,
editing and removing entries, suggests the names in every group field, and
shows them next to addresses; `mcast tui` has a Phonebook tab, and ←→ on a
form's Group field picks a name; API requests accept a name for `group` too.

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

### Playback on real networks

The receiver copes with what multicast networks actually do:

- **Out-of-order packets** are put back in sequence before decoding. A
  missing packet is waited for up to half the playout latency (`--latency`,
  40 ms by default, so 20 ms) before it counts as lost and is concealed.
  Stateful codecs (G.722, Opus) are always decoded in order.
- **Clock drift:** the sender's clock and your sound card's never run at
  quite the same rate. The receiver measures the difference and resamples by
  up to ±1000 ppm (inaudible) to keep the buffer at its target, so
  long-running streams don't glitch every few minutes. It locks on within two
  or three minutes of a new sender. (Recording to WAV keeps the sender's
  timing, so it isn't resampled.)

The stats line shows both:

```
packets 2600 lost 0 (concealed 0) reordered 12 late 0 invalid 0 | buffer 39.0ms underruns 0 overflows 0 drift +23ppm
```

`reordered` arrived out of order but in time to play; `late` arrived too
late (raise `--latency` if this climbs); `drift` is how fast the sender's
clock runs relative to the sound card.

### RTCP: heartbeat and sync

Senders and receivers also talk RTCP (RFC 3550) on the RTP port + 1
(5005 for a stream on 5004). This is the same port ffmpeg's RTP output, and
so `mcast-play.sh`, has always used.

- **Senders** send a Sender Report about every 5 s. It maps the stream's
  RTP timestamps to the sender's wall clock and includes the stream name. When
  the stream ends they send a BYE.
- **Receivers** (`mcast audio recv`) send Receiver Reports with their loss
  and jitter, their name (`-n Kitchen`, default the hostname), and how far
  behind the sender they're playing.

```bash
mcast audio send --file page.wav -e g711u -n "Page"
mcast audio recv --stream page -n Kitchen
#   rtcp: ssrc 319ca154 is "Page" (mcast@10.0.0.5)
#   packets 377 lost 0 ... | transit 1.2ms, playing 41.2ms behind sender
#   rtcp: ssrc 319ca154 ended the stream
```

"Transit" compares the arrival time against the sender's clock, so it is only
the real network delay if both machines' clocks are in sync (NTP gets within a
few ms; PTP within µs). Without synced clocks it shows the difference between
the clocks instead, which is still steady unless the clocks drift.
"Playing behind" adds the audio waiting in the playout buffer. It doesn't
include the sound card's own latency, which isn't visible to us.

| Option | Meaning |
|---|---|
| `--no-rtcp` | Don't send or listen for RTCP |
| `--rtcp-interval SECS` | Average time between reports (default 5; each is randomised ±50% as RFC 3550 asks) |
| `-n, --name` | (`recv`) Name to report, e.g. the room |

RTCP is a few hundred bytes every few seconds. If your phones page on
consecutive ports (e.g. channel 1 on 10000, channel 2 on 10001), channel 1's
RTCP lands on channel 2's port. Phones should drop it, since it looks like an
RTP packet with payload type 72, which no codec uses. `mcast` receivers
recognise it and skip it. To be safe, `--no-rtcp` stops it being sent at all.

Defaults: group `239.69.0.1:5004`, 48 kHz, 2 channels, L16, 1 ms packets, TTL 16.

Network options:

| Option | Meaning |
|---|---|
| `-g, --group` | Group and port, IPv4 or IPv6 (`[ff15::69]:5004`) |
| `-i, --iface` | IPv4 interface address, or interface index for IPv6 |
| `--ttl` | TTL / hop limit |
| `--no-loopback` | Don't deliver to receivers on the same host |

## Text

Real-time text over RTP: T.140 per RFC 4103, the format IP phones and
accessibility (TTY replacement) systems use for text calls. Text is gathered
for up to 300 ms per packet, and each packet repeats the previous two packets'
text (RFC 2198 redundancy). A receiver can therefore lose up to two packets in
a row and still rebuild the text. Text lost beyond that shows as `�`. Nothing
is sent while nobody types. RTCP (above) keeps the heartbeat going and carries
each sender's name.

```bash
mcast text send "fire drill at 3pm"            # one message, announced via SAP as "<host> text"
mcast text send -n Ops < notices.txt          # each line of a file
tail -f app.log | mcast text send -n "App log" # follow a log
mcast text recv                               # print the first text stream announced
mcast text recv --stream ops                  # the one whose name contains "ops"
mcast text recv -g 239.69.1.1:5004            # skip discovery
mcast text chat -n Alice                      # send what you type, print everyone else
```

`recv` prints whole lines with the sender's name (`Ops: fire drill at 3pm`).
A half-typed line is shown once its sender has been quiet for 3 s.
`--raw` prints the characters as they arrive, with no names.

`chat` is send and receive on one group (default `239.69.1.1:5004`, kept away
from the audio and paging groups), without SAP. Everyone who runs
`mcast text chat` on the same group sees each other's lines:

```
chatting on 239.69.1.1:5004 as "Bob". Type and press Enter; Ctrl-D or Ctrl-C to leave.
[Alice is here]
Alice: hi bob
Alice: how's the lobby?
[Alice left]
```

| Option | Meaning |
|---|---|
| `-n, --name` | Name shown to receivers (sent via SAP and RTCP) |
| `-g, --group` | Group and port [default `239.69.1.1:5004`] |
| `--no-red` | Plain T.140, without redundant copies |
| `--pt`, `--red-pt` | Payload types [default 98 for T.140, 100 for redundancy, as in RFC 4103's examples] |
| `--no-sap`, `--sap-group`, `--no-rtcp`, `--tos`, `--ttl`, `-i` | As for audio |

With SAP on, `send` waits 1 s before the first text so receivers that
discover the stream have time to join. `mcast audio discover` lists text
streams too, and `mcast monitor` shows them (status "active", or "idle Ns"
between messages).

## Files and pictures

Send any file, such as a picture, to everyone on a group at once:

```bash
mcast file send sunset.jpg                     # announced via SAP as "<host> files"
mcast file send *.png --rate 5000              # several, one after another, at 5 Mbit/s
mcast file send poster.jpg --loop              # keep sending until Ctrl-C, for late joiners
mcast file recv                                # save whatever arrives in this directory
mcast file recv --show --once                  # show the first picture in the terminal, then exit
mcast file recv -o ~/inbox --stream photos     # from the "Photos" sender, into ~/inbox
mcast file show sunset.jpg                     # just the terminal preview, no network
```

```
receiving fractal.jpg (271.6 KB) from 10.0.0.5
got fractal.jpg (271.6 KB) in 1.6s, from 199 packets (199 needed), checksum OK; saved ./fractal.jpg
```

Multicast has no retransmission, so files are sent with a **fountain code**
(RaptorQ, RFC 6330). The sender sends the file's pieces, then extra "repair"
pieces. A receiver can rebuild the file from *any* set of pieces slightly
larger than the file, whichever ones it happened to catch. With the default
`--redundancy 0.5` the sender sends 1.5× the file, so every receiver can lose
up to about a third of the packets, each a different third, and still get
the file. That was tested with 25% random loss. With `--loop` the sender keeps
making new repair pieces, so a receiver that joins late finishes as soon as it
has caught enough.

- `--show` draws pictures (PNG, JPEG, GIF, WebP, BMP) with coloured
  half-block characters, which works in any 24-bit-colour terminal.
- Each file carries a CRC-32. A file that fails the check isn't saved.
- Received names are reduced to a plain file name: no directories, no
  leading dots, no control characters. Existing files aren't overwritten:
  `cat.jpg` becomes `cat (1).jpg`.
- `--max-size` (default 512 MB) refuses bigger files before allocating
  anything. Malformed packets are ignored.
- A file that stops arriving is given up on after 30 s.

| Option | Meaning |
|---|---|
| `--rate KBIT` | Sending rate (default 2000). Mind voice traffic on the same network |
| `-r, --redundancy` | Extra repair data as a fraction of the file (default 0.5) |
| `--loop` | Keep sending the last file until Ctrl-C |
| `-o, --out DIR` | Where to save (default `.`); `--no-save` to only look |
| `--show`, `--width COLS` | Show pictures in the terminal |
| `--once` | Exit after the first file |
| `-g, --group` | Default `239.69.2.1:5004`, away from audio, paging and text |

Files travel as RTP (payload type 110, `x-mcast-file`), so `mcast monitor`
shows their rate and loss. They also carry RTCP, so the sender's heartbeat and
each receiver's loss show up too. The payload format is documented in
`crates/mcast-file/src/wire.rs`. It's specific to mcast-tools; the standard
for broadcast file delivery, FLUTE (RFC 6726), is a lot more machinery.

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

The monitor also listens on each group's RTCP port and adds lines under each
stream: the sender's heartbeat, and everyone listening:

```
Page 239.69.0.11:5004          10.0.0.5:59325        PCMU/8000/1      50.0     68.8   0.0%     0   0.1ms 20.0ms   16 EF    ██████████  -2.4 dBFS
                               └ rtcp: "Page", SR every 5.0s, last 2s ago, one-way 0.4ms, media clock +12 ppm
                               └ listener Kitchen (10.0.0.21): loss 0.0%, jitter 0.2ms, plays 41.3ms behind (transit 1.3 + buffer 40.0), heard 1s ago
                               └ listener Lobby (10.0.0.22): loss 0.4%, jitter 1.1ms, plays 43.9ms behind (transit 3.9 + buffer 40.0), heard 3s ago
                               └ sync: 2 listeners within 2.6ms of each other
```

- **SR … last Ns ago**: the sender's heartbeat. "(reports stopped)" appears
  if none arrive for 15 s, and the status says "ended (BYE)" when the sender
  signs off.
- **one-way**: when the last SR arrived minus its timestamp: the network
  delay if the clocks are synced, otherwise the offset between them.
- **media clock**: how fast the stream's sample clock runs against the
  sender's wall clock, after 10 s of reports. A capture card that's 50 ppm
  fast gains 180 ms an hour, and receivers have to absorb that.
- **listener**: any RTP receiver that sends reports, with its loss and
  jitter. `mcast audio recv` also reports how far behind it plays. The
  **sync** line shows the spread across listeners, which tells you whether
  rooms are hearing the same thing at the same time.

These delay figures are only as good as the clocks they're measured with.
Listeners are forgotten 30 s after their last report.

## Web GUI and API server

`mcast serve` runs everything above as a long-lived service, with a web GUI
at `/` and an HTTP/JSON API under `/api/v1` for home automation or scripts on
other machines:

```bash
mcast serve                                          # open http://127.0.0.1:8650/
MCAST_TOKEN=change-me mcast serve --listen 0.0.0.0:8650
MCAST_TOKEN=change-me MCAST_LISTEN=0.0.0.0:8650 mcast serve   # the same, from the environment
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
Listening on anything but loopback (`--listen` or `MCAST_LISTEN`) requires a token. See
[docs/api.md](docs/api.md) for every route and field.

A list of files to play can include text to speak (`{"say": "The library
closes in ten minutes"}`). `mcast serve` speaks it with Piper when it's
installed with a voice (`--tts-voices DIR`), else eSpeak NG, or any command
you give it (`--tts-command`, with `--tts-voice-list` naming its voices), and
keeps what it spoke so the same words are synthesized once. It says at start which one it found.

The GUI pages from uploaded files, live input, a test tone or the chord pad;
chats on real-time text channels; sends files and pictures and receives them
into its media list (with thumbnails); shows what's playing with a stop
button; lists announced streams with Listen, Join and Receive buttons; and
shows the monitor with level meters; and keeps the phonebook of named groups.
It asks for the token if one is set.
`--no-web` serves the API alone; `--web-dir crates/mcast-web/assets` serves
the GUI's files from disk, so edits show up on reload without rebuilding.

`mcast tui` does the same from a terminal, so it works over SSH:

```bash
ssh pager mcast tui                                  # the server on that box
MCAST_TOKEN=change-me mcast tui --url http://pager:8650
```

Tabs for what's playing, text chat, announced streams, the monitor,
media (send and receive files there too) and the phonebook (add, edit and
remove entries; Enter pages one); `p` opens the page form, `?` lists the
keys. In any form, ←→ on the Group field steps through phonebook names.
Like the web GUI, it's a client of the API: it needs `mcast serve` running,
and every client sees the same jobs.

### Intercom

`mcast intercom` plays announcements to zones of speakers through `mcast serve`.
A zone is a set of groups, each with the codec its speakers need. An announcement
is an audio file or a chime, plus the zones it goes to, how many times to play it,
and a priority that decides whether it may take over a group something else is
using. They're kept in `~/.config/mcast/intercom.json`:

```bash
mcast intercom serve                                 # the panel and schedules: http://127.0.0.1:8651/
mcast intercom list
mcast intercom check                                 # media files and phonebook names exist?
mcast intercom upcoming                              # what the schedules play next
mcast intercom play "Lunch bell"                     # Ctrl-C stops it
mcast intercom token add Doorbell --role trigger     # a key for another system
mcast intercom backup --sounds                       # the config and every sound, as a .tar.gz
mcast intercom restore intercom-2026-11-08.tar.gz --dry-run
```

The panel has a button for each announcement, what's playing now and next,
recent plays, editors for schedules, announcements and zones, and a Settings
tab. There, default chimes and sound settings apply to every announcement
that doesn't set its own. Schedules play weekly or once at wall-clock times
in a time zone you choose, and only while the panel runs. They can all be
paused at once, skip days off (holidays, breaks), and sit in groups that
switch on and off together. Several sites can each have their own zones,
schedules, time zone and days off. A Sounds tab manages the mcast server's
sound files in folders. Playlists play sounds and text pages in order; a text
page is spoken by `mcast serve`, shown on the zones' text displays (`mcast
text recv`) and on a full-screen display page (`?display=1`), or both, as the
playlist reaches it. Live announcements are push to talk from the
browser's microphone, which browsers only allow over HTTPS:
`--tls-self-signed` (or `--tls-cert`/`--tls-key`) serves the panel that way.
It is its own server with its own token (`--panel-token`); `--url` and
`--token` point it at `mcast serve`.

Since 0.5.5 the panel also has:
- **Access and other systems:**
  - Scoped access tokens (admin, operator, trigger and media), and kiosk screens.
  - A trigger API for other systems, and signed outbound webhooks. Webhooks can be limited to sites, and each keeps a delivery log.
- **The library and playback:**
  - Checked uploads and recording in the browser, and a file manager with tags and search.
  - Playlists, and parts that send other audio to other zones in the same play.
- **The Panel and history:**
  - Categories, colors and an arrangeable layout.
  - A play history kept on disk, and a Monitor tab (for one site or all).
- **Speakers:**
  - Delivery confirmation: which speakers (`mcast receive --name`) heard each play.
  - Alerts when one goes offline between plays.
- **GPIO buttons and door contacts** on a Raspberry Pi.
- **Backup and restore.**
See [crates/mcast-intercom/README.md](crates/mcast-intercom/README.md) for the
file's format and the panel's API. On a Raspberry Pi, `ENABLE_INTERCOM=yes` in
`pi/pi.conf` runs it as a service ([pi/README.md](pi/README.md#the-intercom-panel)).

## Layout

| Crate | Purpose |
|---|---|
| `crates/mcast-core` | Multicast sockets (v4/v6), RTP, RTCP (packets and sender/receiver sessions), SAP announce/directory, SDP parse/write |
| `crates/mcast-audio` | Codecs (L16/L24, G.711, G.722, Opus), voice/music processing (`process`, `dsp`), resamplers, packetizer, reorder and jitter buffers, drift control, sender & receiver, device I/O (cpal) |
| `crates/mcast-text` | Real-time text: T.140 + RFC 2198 redundancy, sender, receiver, line buffering, SDP |
| `crates/mcast-file` | Files over multicast: RaptorQ encoding, packet format, receiver/assembler, terminal picture preview |
| `crates/mcast-monitor` | Stream monitor: per-source rate, loss, jitter, TTL, DSCP, level; RTCP senders and listeners |
| `crates/mcast-control` | Send/receive requests shared by the CLI and the API; jobs, SAP list and monitor as background services |
| `crates/mcast-server` | HTTP/JSON API (`mcast serve`) |
| `crates/mcast-tui` | Terminal front end (`mcast tui`), an API client built on ratatui |
| `crates/mcast-intercom` | Intercom panel (`mcast intercom`): zones, announcements, schedules, its own web page and API; an API client that uses no other mcast crate |
| `crates/mcast-web` | Web GUI: plain HTML/CSS/JS modules in `assets/` (one per panel, no build step), embedded in the binary |
| `crates/mcast` | The `mcast` CLI |
| `scripts/mcast-play.sh` | ffmpeg-based paging script |
| `pi/` | Raspberry Pi appliance build: `mcast serve` as a service, LLDP/CDP, SSH, Cockpit, firewall ([pi/README.md](pi/README.md)) |

New tool families go in their own crate (e.g. `crates/mcast-<thing>`) and get a
subcommand in the CLI.

## Roadmap ideas

- Audio: PTP-aligned timestamps (AES67), playing in step across receivers
  (using the RTCP clock mapping)
- Other: ping/latency, relay/bridge between interfaces

## License

MIT: see [LICENSE](LICENSE).
