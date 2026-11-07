# mcast-tools

A collection of multicast tools, written in Rust. The first set is audio:
uncompressed PCM over RTP multicast, using the same L16/L24 payloads as AES67.

## Build

```bash
cargo build --release
```

On Linux the audio crates need ALSA headers: `sudo apt-get install libasound2-dev pkg-config`.

## Audio

Send a test tone, a WAV file, or live input:

```bash
mcast audio send --tone 440                    # 440 Hz at -12 dBFS
mcast audio send --wav music.wav --loop        # rate/channels taken from the file
mcast audio send --capture                     # default input device
mcast audio send --capture "USB" -g 239.69.0.5:5004 -e l24
```

Receive and play, or record:

```bash
mcast audio recv                               # default group, 48 kHz stereo L16
mcast audio recv --latency 20 --device "Speakers"
mcast audio recv --wav capture.wav --duration 10
```

List devices with `mcast audio devices`.

Sender and receiver must agree on format (`--rate`, `--channels`, `--encoding`);
the sender prints the matching `recv` command. Defaults: group `239.69.0.1:5004`,
48 kHz, 2 channels, L16, 1 ms packets, TTL 16.

Network options shared by all tools:

| Option | Meaning |
|---|---|
| `-g, --group` | Group and port, IPv4 or IPv6 (`[ff15::69]:5004`) |
| `-i, --iface` | IPv4 interface address, or interface index for IPv6 |
| `--ttl` | TTL / hop limit |
| `--no-loopback` | Don't deliver to receivers on the same host |

## Layout

| Crate | Purpose |
|---|---|
| `crates/mcast-core` | Multicast socket setup (v4/v6), RTP header parse/write, sequence/loss tracking |
| `crates/mcast-audio` | PCM encoding, packetizer, jitter buffer, sender & receiver, device I/O (cpal) |
| `crates/mcast` | The `mcast` CLI |

New tool families go in their own crate (e.g. `crates/mcast-<thing>`) and get a
subcommand in the CLI.

## Roadmap ideas

- Audio: SAP/SDP announce & discovery, PTP-aligned timestamps (AES67), Opus, resampling
- Monitor: join groups and show rate, loss, jitter
- Other: file transfer, chat, ping/latency, relay/bridge between interfaces
