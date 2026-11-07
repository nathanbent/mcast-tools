//! `mcast text`: real-time text (T.140) over RTP multicast.

use crate::{hostname, NetArgs, RtcpArgs, SapListenArgs};
use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use mcast_core::rtcp::RtcpConfig;
use mcast_core::sap::{Directory, DirectoryEvent};
use mcast_text::lines::LineBuffer;
use mcast_text::receiver::{self, TextReceiverConfig};
use mcast_text::sender::{self, TextSender, TextSenderConfig};
use mcast_text::t140::{DEFAULT_RED_PT, DEFAULT_T140_PT};
use mcast_text::TextInfo;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Text gets its own default group, away from audio and paging groups.
const DEFAULT_TEXT_GROUP: &str = "239.69.1.1:5004";
/// Show a half-typed line once its sender has been quiet this long.
const STALE_LINE: Duration = Duration::from_secs(3);

#[derive(Subcommand)]
pub enum TextCommand {
    /// Send text: the arguments as one message, or each line typed/piped on stdin
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

/// Feed `input` to the sender until it ends (then flush) or `stop`.
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
                    s.push(&text);
                    // Take whatever else is already typed or piped.
                    while let Ok(more) = input.try_recv() {
                        s.push(&more);
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
    let name = a
        .name
        .clone()
        .unwrap_or_else(|| format!("{} text", hostname()));
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
/// raw characters as they arrive.
struct Printer {
    raw: bool,
    /// Chat: say "joined" by name instead of listing sources.
    chat: bool,
    labels: HashMap<u32, String>,
    lines: HashMap<u32, LineBuffer>,
}

impl Printer {
    fn new(raw: bool, chat: bool) -> Self {
        Self {
            raw,
            chat,
            labels: HashMap::new(),
            lines: HashMap::new(),
        }
    }

    fn line(&self, ssrc: u32, text: &str) {
        let label = self
            .labels
            .get(&ssrc)
            .cloned()
            .unwrap_or_else(|| format!("{ssrc:08x}"));
        println!("{label}: {text}");
    }

    fn event(&mut self, e: receiver::Event<'_>) {
        let now = Instant::now();
        match e {
            receiver::Event::NewSource { ssrc, from } => {
                self.labels
                    .entry(ssrc)
                    .or_insert_with(|| from.ip().to_string());
                if !self.raw && !self.chat {
                    eprintln!("[text from {from}, ssrc {ssrc:08x}]");
                }
            }
            receiver::Event::Named { ssrc, name } => {
                if self.chat && !self.labels.values().any(|l| *l == name) {
                    eprintln!("[{name} is here]");
                }
                self.labels.insert(ssrc, name);
            }
            receiver::Event::Text { ssrc, text } => {
                if self.raw {
                    print!("{text}");
                    let _ = std::io::stdout().flush();
                    return;
                }
                let buf = self
                    .lines
                    .entry(ssrc)
                    .or_insert_with(|| LineBuffer::new(now));
                for line in buf.push(text, now) {
                    self.line(ssrc, &line);
                }
            }
            receiver::Event::Tick => {
                let stale: Vec<(u32, String)> = self
                    .lines
                    .iter_mut()
                    .filter_map(|(&ssrc, b)| b.take_stale(now, STALE_LINE).map(|t| (ssrc, t)))
                    .collect();
                for (ssrc, text) in stale {
                    self.line(ssrc, &text);
                }
            }
            receiver::Event::Bye { ssrc, reason } => {
                if let Some(text) = self.lines.remove(&ssrc).and_then(|mut b| b.take()) {
                    self.line(ssrc, &text);
                }
                if !self.raw {
                    let who = self
                        .labels
                        .get(&ssrc)
                        .cloned()
                        .unwrap_or_else(|| format!("{ssrc:08x}"));
                    match reason {
                        Some(r) => eprintln!("[{who} left: {r}]"),
                        None => eprintln!("[{who} left]"),
                    }
                }
            }
            receiver::Event::UnknownPayloadType { payload_type } => {
                eprintln!("warning: ignoring payload type {payload_type}; check --pt / --red-pt")
            }
            receiver::Event::Warning { message } => eprintln!("warning: {message}"),
        }
    }

    fn finish(&mut self) {
        let left: Vec<(u32, String)> = self
            .lines
            .iter_mut()
            .filter_map(|(&ssrc, b)| b.take().map(|t| (ssrc, t)))
            .collect();
        for (ssrc, text) in left {
            self.line(ssrc, &text);
        }
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
    let name = a.name.clone().unwrap_or_else(hostname);
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
