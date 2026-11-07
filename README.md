# mcast-tools

A collection of multicast tools, written in Rust. The first set is audio:
uncompressed PCM over RTP multicast, using the same L16/L24 payloads as AES67.

## Install

Each [release](https://github.com/nathanbent/mcast-tools/releases) has a
`.deb` and a plain binary (`.tar.gz`) for:

| Package arch | Rust target | For |
|---|---|---|
| `amd64` | `x86_64-unknown-linux-gnu` | PCs and servers |
| `arm64` | `aarch64-unknown-linux-gnu` | Raspberry Pi 3/4/5 on 64-bit Raspberry Pi OS |
| `armhf` | `armv7-unknown-linux-gnueabihf` | Pi 2/3/4/Zero 2 on 32-bit Raspberry Pi OS (needs NEON, so not Pi 1 / Zero) |

They need Ubuntu 22.04+, Debian 12+ or Raspberry Pi OS bookworm or newer
(glibc 2.34+), plus ALSA (`libasound2`, always present on those systems).
Check the download against `SHA256SUMS`, then:

```bash
sudo apt install ./mcast_0.1.0-1_arm64.deb
```

### Run as a service

The package installs a systemd template unit. Each file in `/etc/mcast` is
one instance; its `MCAST_ARGS` line is the `mcast` command line to run:

```bash
sudo cp /usr/share/doc/mcast/examples/mcast.conf.example /etc/mcast/lobby.conf
sudo nano /etc/mcast/lobby.conf      # e.g. MCAST_ARGS=audio recv --stream lobby --device plughw:CARD=Headphones
sudo systemctl enable --now mcast@lobby
journalctl -u mcast@lobby -f
```

Services restart if they exit, start at boot once the network is up, and
run as an unprivileged throwaway user that can reach the network and sound
cards and nothing else. Files they read or write (`--file`, `--wav`) belong in
`/var/lib/mcast`. `MCAST_ARGS` is split on spaces with no quoting, so choose
devices and streams by a part of their name without spaces.

### Making a release

Bump `version` in `Cargo.toml`, commit, then push a matching tag:

```bash
git tag v0.2.0
```

```bash
git push origin v0.2.0
```

The Release workflow builds all three targets in an Ubuntu 22.04 container
and publishes them. To build one yourself (Docker, any Linux host):

```bash
docker run --rm -v "$PWD:/src" -w /src ubuntu:22.04 packaging/build-release.sh aarch64-unknown-linux-gnu
```

## Build

```bash
cargo build --release
```

On Debian/Ubuntu install the build dependencies first (ALSA for audio devices, libopus for Opus):

```bash
sudo apt-get install build-essential pkg-config libasound2-dev libopus-dev
```

## Audio

Send a test tone, an audio file (WAV, MP3, FLAC, Ogg Vorbis, AAC/M4A, ALAC), or live input:

```bash
mcast audio send --tone 440                    # 440 Hz at -12 dBFS
mcast audio send --file music.mp3 --loop       # MP3, FLAC, Ogg, M4A/AAC, WAV...
mcast audio send --capture                     # default input device
mcast audio send --capture "USB" -g 239.69.0.5:5004 -e l24
```

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

With voice or music, a low-pass at the codec's band edge (3.4 kHz G.711,
7 kHz G.722 and wideband Opus) runs before loudness normalisation, and the
true peak is held at -2 dBTP. Opus tuning follows the mode.

| `mcast-play.sh` | `mcast audio send` |
|---|---|
| `-c g711u / g711a / g722 / opus` | `-e g711u / g711a / g722 / opus` |
| `FILE` (anything ffmpeg reads) | `--file FILE` (WAV, MP3, FLAC, Ogg, AAC/M4A, ALAC) |
| `-c opuswb` | `-e opus --opus-bandwidth wb --bitrate 32` |
| `-m voice` / `-p` / `-m music` | `-m voice` / `-p` / `-m music` |
| `-v DB`, `-L MS`, `-s SECS`, `-t SECS` | same |
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

### Playback on real networks

The receiver copes with what multicast networks actually do:

- **Out-of-order packets** are put back in sequence before decoding. A missing
  packet is waited for up to half the playout latency (`--latency`, 40 ms by
  default, so 20 ms) before it counts as lost and is concealed. Stateful
  codecs (G.722, Opus) are always decoded in order.
- **Clock drift**: the sender's clock and your sound card's never run at quite
  the same rate. The receiver measures the difference and resamples by up to
  ±1000 ppm (inaudible) to keep the buffer at its target, so long-running
  streams don't glitch every few minutes. It locks on within two or three
  minutes of a new sender.

The stats line shows both:

```
packets 2600 lost 0 (concealed 0) reordered 12 late 0 invalid 0 | buffer 39.0ms underruns 0 overflows 0 drift +23ppm
```

`reordered` arrived out of order but in time to play; `late` arrived too late
(raise `--latency` if this climbs); `drift` is how fast the sender's clock runs
relative to the sound card.

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

## Layout

| Crate | Purpose |
|---|---|
| `crates/mcast-core` | Multicast sockets (v4/v6), RTP, SAP announce/directory, SDP parse/write |
| `crates/mcast-audio` | Codecs (L16/L24, G.711, G.722, Opus), voice/music processing (`process`, `dsp`), resamplers, packetizer, reorder and jitter buffers, drift control, sender & receiver, device I/O (cpal) |
| `crates/mcast-monitor` | Stream monitor: per-source rate, loss, jitter, TTL, DSCP, level |
| `crates/mcast` | The `mcast` CLI |

New tool families go in their own crate (e.g. `crates/mcast-<thing>`) and get a
subcommand in the CLI.

## Roadmap ideas

- Audio: PTP-aligned timestamps (AES67)
- Other: file transfer, chat, ping/latency, relay/bridge between interfaces
