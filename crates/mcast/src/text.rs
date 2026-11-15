//! `mcast text`: real-time text (T.140) over RTP multicast.

use crate::{hostname, NetArgs, RtcpArgs, SapListenArgs};
use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use mcast_core::rtcp::RtcpConfig;
use mcast_core::sap::{Directory, DirectoryEvent};
use mcast_text::receiver::{self, TextReceiverConfig};
use mcast_text::sender::{self, TextSender, TextSenderConfig};
use mcast_text::t140::{BACKSPACE, DEFAULT_RED_PT, DEFAULT_T140_PT};
use mcast_text::{Entry, TextInfo, Transcript, DEFAULT_GROUP as DEFAULT_TEXT_GROUP};
use std::io::{BufRead, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Subcommand)]
pub enum TextCommand {
    /// Send text: the arguments as one message, or each line typed/piped on stdin.
    /// Emoji work, typed or as shortcodes (:fire: :warning: :bell:)
    Send(TextSendArgs),
    /// Print text from a stream; finds one via SAP unless --group is given
    Recv(TextRecvArgs),
    /// Chat on a group: send what you type, print what everyone else sends
    Chat(TextChatArgs),
}

#[derive(Args)]
struct SenderArgs {
    #[command(flatten)]
    net: NetArgs,
    /// IP TOS byte for the text packets
    #[arg(long)]
    tos: Option<u32>,
    /// Send plain T.140, without copies of the previous packets' text (RFC 2198 redundancy)
    #[arg(long)]
    no_red: bool,
    /// T.140 payload type
    #[arg(long, default_value_t = DEFAULT_T140_PT)]
    pt: u8,
    /// Redundancy (RED) payload type
    #[arg(long, default_value_t = DEFAULT_RED_PT)]
    red_pt: u8,
    #[command(flatten)]
    rtcp: RtcpArgs,
}

impl SenderArgs {
    fn sender(
        &self,
        group: SocketAddr,
        name: String,
        announce: Option<sender::Announce>,
    ) -> Result<TextSender> {
        let mut net = self.net.options(group)?;
        net.tos = self.tos;
        TextSender::new(TextSenderConfig {
            net,
            t140_pt: self.pt,
            red: (!self.no_red).then_some((self.red_pt, 2)),
            interval: sender::DEFAULT_INTERVAL,
            announce,
            rtcp: self.rtcp.interval().map(|interval| RtcpConfig {
                name: Some(name),
                interval,
            }),
        })
    }
}

#[derive(Args)]
pub struct TextSendArgs {
    /// Message to send (else lines from stdin)
    message: Vec<String>,
    /// Multicast group and port, or a phonebook name
    #[arg(short, long, default_value = DEFAULT_TEXT_GROUP, value_parser = crate::book::parse_group)]
    group: SocketAddr,
    /// Name announced to receivers [default: "<hostname> text"]
    #[arg(short, long)]
    name: Option<String>,
    /// Don't announce the stream with SAP
    #[arg(long)]
    no_sap: bool,
    /// SAP group to announce on
    #[arg(long, value_name = "ADDR:PORT")]
    sap_group: Option<SocketAddr>,
    /// Seconds between SAP announcements
    #[arg(long, default_value_t = 5.0)]
    sap_interval: f64,
    #[command(flatten)]
    sender: SenderArgs,
}

#[derive(Args)]
pub struct TextRecvArgs {
    /// Multicast group and port, or a phonebook name; skips SAP discovery
    #[arg(short, long, value_parser = crate::book::parse_group)]
    group: Option<SocketAddr>,
    /// Pick the announced text stream whose name contains this text
    #[arg(short, long, conflicts_with = "group")]
    stream: Option<String>,
    /// Seconds to wait for a SAP announcement
    #[arg(short, long, default_value_t = 15.0)]
    wait: f64,
    #[command(flatten)]
    sap: SapListenArgs,
    #[command(flatten)]
    net: NetArgs,
    /// T.140 payload type (with --group)
    #[arg(long, default_value_t = DEFAULT_T140_PT)]
    pt: u8,
    /// Redundancy (RED) payload type (with --group)
    #[arg(long, default_value_t = DEFAULT_RED_PT)]
    red_pt: u8,
    /// Print text the moment it arrives, without names or line buffering
    #[arg(long)]
    raw: bool,
    /// Print one JSON object per line instead, for another program to read:
    /// {"type": "line", "from", "ssrc", "text", "at_ms"}, and "joined", "named", "left", "warning"
    #[arg(long, conflicts_with = "raw")]
    json: bool,
    /// Stop after this many seconds
    #[arg(short, long)]
    duration: Option<f64>,
    /// Name to report ourselves as over RTCP [default: hostname]
    #[arg(short, long)]
    name: Option<String>,
    #[command(flatten)]
    rtcp: RtcpArgs,
}

#[derive(Args)]
pub struct TextChatArgs {
    /// Multicast group and port everyone chats on, or a phonebook name
    #[arg(short, long, default_value = DEFAULT_TEXT_GROUP, value_parser = crate::book::parse_group)]
    group: SocketAddr,
    /// Your name, shown to the others [default: hostname]
    #[arg(short, long)]
    name: Option<String>,
    #[command(flatten)]
    sender: SenderArgs,
}

pub fn run(cmd: TextCommand, stop: &AtomicBool) -> Result<()> {
    match cmd {
        TextCommand::Send(a) => send(a, stop),
        TextCommand::Recv(a) => recv(a, stop),
        TextCommand::Chat(a) => chat(a, stop),
    }
}

/// The name we were given, as the others see it: on one line and safe to show.
fn own_name(name: &Option<String>) -> Option<String> {
    name.as_deref()
        .map(mcast_plain::label)
        .filter(|n| !n.is_empty())
}

/// Lines from stdin, read on a thread so sending isn't blocked on typing.
fn stdin_lines() -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(line + "\n").is_err() {
                break;
            }
        }
    });
    rx
}

/// Feed `input` to the sender until it ends (then flush) or `stop`. What's
/// typed goes out as [`mcast_plain::shown`] makes it: shortcodes as emoji,
/// and nothing a display shouldn't be sent (a log's colour codes, say).
fn drive(s: &mut TextSender, input: mpsc::Receiver<String>, stop: &AtomicBool) -> Result<()> {
    let mut open = true;
    while !stop.load(Ordering::Relaxed) && (open || !s.idle()) {
        let now = Instant::now();
        let wait = s
            .next_wake()
            .map_or(Duration::MAX, |t| t.saturating_duration_since(now))
            .min(Duration::from_millis(200));
        if open {
            match input.recv_timeout(wait) {
                Ok(text) => {
                    s.push(&mcast_plain::shown(&text));
                    // Take whatever else is already typed or piped.
                    while let Ok(more) = input.try_recv() {
                        s.push(&mcast_plain::shown(&more));
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => open = false,
            }
        } else {
            std::thread::sleep(wait);
        }
        s.poll()?;
    }
    Ok(())
}

fn send(a: TextSendArgs, stop: &AtomicBool) -> Result<()> {
    let name = own_name(&a.name).unwrap_or_else(|| format!("{} text", hostname()));
    let announce = (!a.no_sap).then(|| sender::Announce {
        name: name.clone(),
        sap_group: a.sap_group,
        interval: Duration::from_secs_f64(a.sap_interval.max(0.5)),
    });
    let mut s = a.sender.sender(a.group, name, announce)?;
    match s.sap_group() {
        Some(g) => eprintln!(
            "sending text to {} as \"{}\"; announced via SAP on {g}",
            a.group,
            s.sdp().name
        ),
        None => eprintln!("sending text to {}", a.group),
    }
    if let Some(g) = s.rtcp_group() {
        eprintln!("RTCP sender reports on {g}");
    }
    let rx = if a.message.is_empty() {
        eprintln!("type lines to send; Ctrl-D to finish.");
        stdin_lines()
    } else {
        let (tx, rx) = mpsc::channel();
        tx.send(a.message.join(" ") + "\n")?;
        rx
    };
    drive(&mut s, rx, stop)?;
    let st = s.stats();
    eprintln!("sent {} characters in {} packets", st.chars, st.packets);
    Ok(())
}

/// Wait for a SAP-announced text stream, optionally matching `name`.
fn find_text_stream(
    dir: &mut Directory,
    name: Option<&str>,
    wait: Duration,
    stop: &AtomicBool,
) -> Result<Option<TextInfo>> {
    let wanted = name.map(str::to_lowercase);
    let deadline = Instant::now() + wait;
    while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
        for event in dir.poll(deadline.saturating_duration_since(Instant::now()))? {
            let (DirectoryEvent::Added(s) | DirectoryEvent::Changed(s)) = event else {
                continue;
            };
            let Some(info) = TextInfo::from_sdp(&s.sdp) else {
                continue;
            };
            if let Some(w) = &wanted {
                if !info.name.to_lowercase().contains(w) {
                    eprintln!("  skipping {info}");
                    continue;
                }
            }
            return Ok(Some(info));
        }
    }
    Ok(None)
}

/// Prints received text: whole lines labelled with the sender's name, or
/// raw characters as they arrive. Either way it's made safe for a terminal.
struct Printer {
    raw: bool,
    /// A JSON object per line, for another program.
    json: bool,
    /// Chat: say who's here by name instead of listing sources.
    chat: bool,
    transcript: Transcript,
}

impl Printer {
    fn new(raw: bool, chat: bool) -> Self {
        Self {
            raw,
            json: false,
            chat,
            transcript: Transcript::new(),
        }
    }

    fn event(&mut self, e: receiver::Event<'_>) {
        if let (true, receiver::Event::Text { text, .. }) = (self.raw, &e) {
            // Erasing is the one control a terminal gets, as "back, blank, back".
            let pieces: Vec<String> = text.split(BACKSPACE).map(mcast_plain::clean).collect();
            print!("{}", pieces.join("\u{8} \u{8}"));
            let _ = std::io::stdout().flush();
            return;
        }
        let entries = self.transcript.event(e, Instant::now());
        self.show(entries);
    }

    fn finish(&mut self) {
        let entries = self.transcript.finish();
        self.show(entries);
    }

    fn show(&self, entries: Vec<Entry>) {
        if self.json {
            let mut out = std::io::stdout().lock();
            for e in entries {
                let _ = writeln!(out, "{}", entry_json(e));
            }
            let _ = out.flush();
            return;
        }
        for e in entries {
            match e {
                Entry::Line { from, text, .. } => println!("{from}: {text}"),
                Entry::Joined { ssrc, from } if !self.raw && !self.chat => {
                    eprintln!("[text from {from}, ssrc {ssrc:08x}]")
                }
                Entry::Named {
                    name, new: true, ..
                } if self.chat => eprintln!("[{name} is here]"),
                Entry::Joined { .. } | Entry::Named { .. } => {}
                Entry::Left { .. } if self.raw => {}
                Entry::Left {
                    who,
                    reason: Some(r),
                } => eprintln!("[{who} left: {r}]"),
                Entry::Left { who, reason: None } => eprintln!("[{who} left]"),
                Entry::UnknownPayloadType(pt) => {
                    eprintln!("warning: ignoring payload type {pt}; check --pt / --red-pt")
                }
                Entry::Warning(message) => eprintln!("warning: {message}"),
            }
        }
    }
}

/// An entry as `--json` prints it.
fn entry_json(e: Entry) -> serde_json::Value {
    use serde_json::json;
    let ssrc_hex = |ssrc: u32| format!("{ssrc:08x}");
    let at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    match e {
        Entry::Line { ssrc, from, text } => {
            json!({"type": "line", "from": from, "ssrc": ssrc_hex(ssrc), "text": text, "at_ms": at_ms})
        }
        Entry::Joined { ssrc, from } => {
            json!({"type": "joined", "ssrc": ssrc_hex(ssrc), "address": from.to_string(), "at_ms": at_ms})
        }
        Entry::Named { ssrc, name, .. } => {
            json!({"type": "named", "ssrc": ssrc_hex(ssrc), "name": name, "at_ms": at_ms})
        }
        Entry::Left { who, reason } => {
            json!({"type": "left", "from": who, "reason": reason, "at_ms": at_ms})
        }
        Entry::UnknownPayloadType(pt) => json!({
            "type": "warning", "message": format!("ignoring payload type {pt}"), "at_ms": at_ms
        }),
        Entry::Warning(message) => json!({"type": "warning", "message": message, "at_ms": at_ms}),
    }
}

fn recv(a: TextRecvArgs, stop: &AtomicBool) -> Result<()> {
    let (group, t140_pt, red_pt) = match a.group {
        Some(g) => (g, a.pt, Some(a.red_pt)),
        None => {
            match &a.stream {
                Some(n) => eprintln!("looking for a text stream named \"{n}\" via SAP..."),
                None => eprintln!("looking for text streams via SAP..."),
            }
            let mut dir = a.sap.open(a.net.iface)?;
            let wait = Duration::from_secs_f64(a.wait);
            let Some(info) = find_text_stream(&mut dir, a.stream.as_deref(), wait, stop)? else {
                if stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                bail!(
                    "no text stream announced within {}s; pass --group (default {DEFAULT_TEXT_GROUP})",
                    a.wait
                );
            };
            eprintln!("found {info}");
            (info.group, info.t140_pt, info.red_pt)
        }
    };
    eprintln!("joining {group}. Ctrl-C to stop.");
    let mut printer = Printer::new(a.raw, false);
    printer.json = a.json;
    receiver::run(
        TextReceiverConfig {
            net: a.net.options(group)?,
            t140_pt,
            red_pt,
            ignore_ssrc: None,
            rtcp: a.rtcp.interval().map(|interval| RtcpConfig {
                name: Some(a.name.clone().unwrap_or_else(hostname)),
                interval,
            }),
            send_reports: true,
            duration: a.duration.map(Duration::from_secs_f64),
        },
        stop,
        |e| printer.event(e),
    )?;
    printer.finish();
    Ok(())
}

fn chat(a: TextChatArgs, stop: &AtomicBool) -> Result<()> {
    let group = a.group;
    let name = own_name(&a.name).unwrap_or_else(hostname);
    let mut s = a.sender.sender(group, name.clone(), None)?;
    let cfg = TextReceiverConfig {
        net: a.sender.net.options(group)?,
        t140_pt: a.sender.pt,
        red_pt: Some(a.sender.red_pt),
        // Don't print our own lines back.
        ignore_ssrc: Some(s.ssrc()),
        // Hear the others' names; our sender already reports for us.
        rtcp: a.sender.rtcp.interval().map(|interval| RtcpConfig {
            name: None,
            interval,
        }),
        send_reports: false,
        duration: None,
    };
    eprintln!(
        "chatting on {group} as \"{name}\". Type and press Enter; Ctrl-D or Ctrl-C to leave."
    );
    // Set when we stop sending (Ctrl-D, or Ctrl-C which `drive` also sees).
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| -> Result<()> {
        let rx = scope.spawn(|| {
            let mut printer = Printer::new(false, true);
            let result = receiver::run(cfg, &done, |e| printer.event(e));
            printer.finish();
            result
        });
        let sent = drive(&mut s, stdin_lines(), stop);
        done.store(true, Ordering::Relaxed);
        rx.join().expect("receiver thread panicked")?;
        sent
    })
}
