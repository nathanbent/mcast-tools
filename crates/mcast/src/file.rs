//! `mcast file`: files and pictures over multicast (RaptorQ fountain code).

use crate::{hostname, NetArgs, RtcpArgs, SapListenArgs};
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use mcast_core::rtcp::RtcpConfig;
use mcast_core::sap::{Directory, DirectoryEvent};
use mcast_file::receiver::{self, safe_file_name, Event, FileReceiverConfig};
use mcast_file::sender::{self, FileSender, FileSenderConfig};
use mcast_file::wire::DEFAULT_PT;
use mcast_file::{preview, FileInfo};
use std::collections::HashMap;
use std::io::IsTerminal;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Files get their own default group, away from audio, paging and text.
const DEFAULT_FILE_GROUP: &str = "239.69.2.1:5004";

#[derive(Subcommand)]
pub enum FileCommand {
    /// Send files (pictures, anything) to a group
    Send(FileSendArgs),
    /// Receive files into a directory; finds a sender via SAP unless --group is given
    Recv(FileRecvArgs),
    /// Show a picture in the terminal (what `recv --show` does), no network involved
    Show(ShowArgs),
}

#[derive(Args)]
pub struct FileSendArgs {
    /// Files to send, one after another
    #[arg(required = true)]
    files: Vec<PathBuf>,
    /// Multicast group and port
    #[arg(short, long, default_value = DEFAULT_FILE_GROUP)]
    group: SocketAddr,
    #[command(flatten)]
    net: NetArgs,
    /// Sending rate in kbit/s. Keep it modest on networks that also carry voice
    #[arg(long, default_value_t = 2000)]
    rate: u32,
    /// Extra repair data on top of each file, as a fraction of its size
    /// (0.5 sends 1.5x the file; receivers survive losing about a third)
    #[arg(short, long, default_value_t = 0.5)]
    redundancy: f64,
    /// Keep sending the (last) file until Ctrl-C, so late joiners get it too
    #[arg(long = "loop")]
    looped: bool,
    /// IP TOS byte for the file packets
    #[arg(long)]
    tos: Option<u32>,
    /// RTP payload type
    #[arg(long, default_value_t = DEFAULT_PT)]
    pt: u8,
    /// Name announced to receivers [default: "<hostname> files"]
    #[arg(short, long)]
    name: Option<String>,
    /// Don't announce with SAP
    #[arg(long)]
    no_sap: bool,
    /// SAP group to announce on
    #[arg(long, value_name = "ADDR:PORT")]
    sap_group: Option<SocketAddr>,
    /// Seconds between SAP announcements
    #[arg(long, default_value_t = 5.0)]
    sap_interval: f64,
    #[command(flatten)]
    rtcp: RtcpArgs,
}

#[derive(Args)]
pub struct FileRecvArgs {
    /// Multicast group and port; skips SAP discovery
    #[arg(short, long)]
    group: Option<SocketAddr>,
    /// Pick the announced file stream whose name contains this text
    #[arg(short, long, conflicts_with = "group")]
    stream: Option<String>,
    /// Seconds to wait for a SAP announcement
    #[arg(short, long, default_value_t = 15.0)]
    wait: f64,
    #[command(flatten)]
    sap: SapListenArgs,
    #[command(flatten)]
    net: NetArgs,
    /// Directory to save files in
    #[arg(short, long, default_value = ".")]
    out: PathBuf,
    /// Show pictures in the terminal as they arrive
    #[arg(long)]
    show: bool,
    /// Width in columns for --show [default: the terminal's]
    #[arg(long)]
    width: Option<u32>,
    /// Exit after the first file
    #[arg(long)]
    once: bool,
    /// Don't save files (with --show: just look)
    #[arg(long)]
    no_save: bool,
    /// Refuse files larger than this many MB
    #[arg(long, default_value_t = 512)]
    max_size: u64,
    /// RTP payload type (with --group)
    #[arg(long, default_value_t = DEFAULT_PT)]
    pt: u8,
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
pub struct ShowArgs {
    /// Picture to show (PNG, JPEG, GIF, WebP, BMP)
    file: PathBuf,
    /// Width in columns [default: the terminal's]
    #[arg(long)]
    width: Option<u32>,
}

pub fn run(cmd: FileCommand, stop: &AtomicBool) -> Result<()> {
    match cmd {
        FileCommand::Send(a) => send(a, stop),
        FileCommand::Recv(a) => recv(a, stop),
        FileCommand::Show(a) => {
            let data =
                std::fs::read(&a.file).with_context(|| format!("reading {}", a.file.display()))?;
            let Some(img) = preview::decode(&data) else {
                bail!("{} isn't a picture we can decode", a.file.display());
            };
            print!("{}", render(&img, a.width));
            Ok(())
        }
    }
}

/// The terminal's size in (columns, rows), if stdout is one.
fn terminal_size() -> Option<(u32, u32)> {
    #[cfg(unix)]
    {
        // SAFETY: TIOCGWINSZ fills a winsize struct we own.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
        if ok && ws.ws_col > 0 {
            return Some((ws.ws_col as u32, ws.ws_row as u32));
        }
    }
    let cols = std::env::var("COLUMNS").ok()?.parse().ok()?;
    let rows = std::env::var("LINES")
        .ok()
        .and_then(|l| l.parse().ok())
        .unwrap_or(24);
    Some((cols, rows))
}

fn render(img: &image::DynamicImage, width: Option<u32>) -> String {
    let (cols, rows) = terminal_size().unwrap_or((80, 24));
    let cols = width.unwrap_or(cols.min(120));
    // Leave room for the status line; let explicit widths decide the height.
    let rows = if width.is_some() {
        u32::MAX / 4
    } else {
        rows.saturating_sub(2).max(8)
    };
    preview::render(img, cols, rows)
}

fn human(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

fn send(a: FileSendArgs, stop: &AtomicBool) -> Result<()> {
    // Read everything first so a bad path fails before we start announcing.
    let mut files = Vec::new();
    for path in &a.files {
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        files.push((name, data));
    }
    let name = a
        .name
        .clone()
        .unwrap_or_else(|| format!("{} files", hostname()));
    let mut net = a.net.options(a.group)?;
    net.tos = a.tos;
    let mut s = FileSender::new(FileSenderConfig {
        net,
        payload_type: a.pt,
        rate_kbps: a.rate,
        redundancy: a.redundancy,
        looped: false,
        announce: (!a.no_sap).then(|| sender::Announce {
            name: name.clone(),
            sap_group: a.sap_group,
            interval: Duration::from_secs_f64(a.sap_interval.max(0.5)),
        }),
        rtcp: a.rtcp.interval().map(|interval| RtcpConfig {
            name: Some(name),
            interval,
        }),
    })?;
    match s.sap_group() {
        Some(g) => eprintln!(
            "sending to {} at {} kbit/s as \"{}\"; announced via SAP on {g}",
            a.group,
            a.rate,
            s.sdp().name
        ),
        None => eprintln!("sending to {} at {} kbit/s", a.group, a.rate),
    }
    let count = files.len();
    let tty = std::io::stderr().is_terminal();
    for (i, (name, data)) in files.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let last = i + 1 == count;
        // Only the last file loops; the others are sent once.
        s.set_looped(last && a.looped);
        let secs = data.len() as f64 * (1.0 + a.redundancy) * 8.0 / (a.rate as f64 * 1000.0);
        eprintln!(
            "{name} ({}){}",
            human(data.len() as u64),
            if last && a.looped {
                " — looping until Ctrl-C".to_string()
            } else {
                format!(", about {secs:.1}s")
            }
        );
        let started = Instant::now();
        let sent = s.send_file(name, data, stop, |p| {
            if !tty {
                return;
            }
            match p.total {
                Some(t) => eprint!(
                    "\r  {:>3}% ({}/{} packets)",
                    p.sent * 100 / t.max(1),
                    p.sent,
                    t
                ),
                None => eprint!(
                    "\r  {} packets ({:.1}x the file)",
                    p.sent,
                    p.sent as f64 / p.source.max(1) as f64
                ),
            }
        })?;
        if tty {
            eprint!("\r\x1b[K");
        }
        eprintln!(
            "  sent {sent} packets in {:.1}s",
            started.elapsed().as_secs_f64()
        );
    }
    Ok(())
}

/// Wait for a SAP-announced file stream, optionally matching `name`.
fn find_file_stream(
    dir: &mut Directory,
    name: Option<&str>,
    wait: Duration,
    stop: &AtomicBool,
) -> Result<Option<FileInfo>> {
    let wanted = name.map(str::to_lowercase);
    let deadline = Instant::now() + wait;
    while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
        for event in dir.poll(deadline.saturating_duration_since(Instant::now()))? {
            let (DirectoryEvent::Added(s) | DirectoryEvent::Changed(s)) = event else {
                continue;
            };
            let Some(info) = FileInfo::from_sdp(&s.sdp) else {
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

/// A path in `dir` for `name` that doesn't exist yet: "cat.jpg", "cat (1).jpg", ...
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    if !path.exists() {
        return path;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (1..)
        .map(|i| dir.join(format!("{stem} ({i}){ext}")))
        .find(|p| !p.exists())
        .expect("some name is free")
}

fn recv(a: FileRecvArgs, stop: &AtomicBool) -> Result<()> {
    let (group, payload_type) = match a.group {
        Some(g) => (g, a.pt),
        None => {
            match &a.stream {
                Some(n) => eprintln!("looking for a file stream named \"{n}\" via SAP..."),
                None => eprintln!("looking for file streams via SAP..."),
            }
            let mut dir = a.sap.open(a.net.iface)?;
            let wait = Duration::from_secs_f64(a.wait);
            let Some(info) = find_file_stream(&mut dir, a.stream.as_deref(), wait, stop)? else {
                if stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                bail!(
                    "no file stream announced within {}s; pass --group (default {DEFAULT_FILE_GROUP})",
                    a.wait
                );
            };
            eprintln!("found {info}");
            (info.group, info.payload_type)
        }
    };
    if !a.no_save {
        std::fs::create_dir_all(&a.out).with_context(|| format!("creating {}", a.out.display()))?;
    }
    eprintln!(
        "joining {group}{}. Ctrl-C to stop.",
        if a.no_save {
            String::new()
        } else {
            format!("; saving to {}", a.out.display())
        }
    );
    let tty = std::io::stderr().is_terminal();
    let mut names: HashMap<u32, String> = HashMap::new();
    let mut failure: Option<anyhow::Error> = None;
    receiver::run(
        FileReceiverConfig {
            net: a.net.options(group)?,
            payload_type: Some(payload_type),
            max_size: a.max_size.saturating_mul(1 << 20),
            rtcp: a.rtcp.interval().map(|interval| RtcpConfig {
                name: Some(a.name.clone().unwrap_or_else(hostname)),
                interval,
            }),
            duration: a.duration.map(Duration::from_secs_f64),
        },
        stop,
        |e| {
            match e {
                Event::Started {
                    object,
                    from,
                    size,
                    name,
                } => {
                    let label = name.clone().unwrap_or_else(|| format!("file {object:08x}"));
                    eprintln!("receiving {label} ({}) from {}", human(size), from.ip());
                    if let Some(n) = name {
                        names.insert(object, n);
                    }
                }
                Event::Named { object, name } => {
                    names.insert(object, name);
                }
                Event::Progress {
                    object,
                    received,
                    source,
                } => {
                    if tty {
                        let label = names
                            .get(&object)
                            .cloned()
                            .unwrap_or_else(|| format!("{object:08x}"));
                        eprint!(
                            "\r  {label}: {}% of what's needed",
                            (received * 100 / source.max(1)).min(99)
                        );
                    }
                }
                Event::Completed(c) => {
                    if tty {
                        eprint!("\r\x1b[K");
                    }
                    let name = c.meta.as_ref().map_or_else(
                        || format!("mcast-file-{:08x}", c.object),
                        |m| m.name.clone(),
                    );
                    let check = match c.verified {
                        Some(true) => "checksum OK",
                        Some(false) => "CHECKSUM MISMATCH",
                        None => "no checksum (name never arrived)",
                    };
                    let mut line = format!(
                        "got {name} ({}) in {:.1}s, from {} packets ({} needed), {check}",
                        human(c.data.len() as u64),
                        c.elapsed.as_secs_f64(),
                        c.received,
                        c.source
                    );
                    if !a.no_save && c.verified != Some(false) {
                        let path = unique_path(&a.out, &safe_file_name(&name, c.object));
                        match std::fs::write(&path, &c.data) {
                            Ok(()) => line += &format!("; saved {}", path.display()),
                            Err(e) => {
                                failure = Some(
                                    anyhow::Error::from(e)
                                        .context(format!("writing {}", path.display())),
                                );
                                return false;
                            }
                        }
                    }
                    eprintln!("{line}");
                    if a.show {
                        match preview::decode(&c.data) {
                            Some(img) => print!("{}", render(&img, a.width)),
                            None => eprintln!("  (not a picture)"),
                        }
                    }
                    if a.once {
                        return false;
                    }
                }
                Event::Abandoned {
                    object,
                    name,
                    received,
                    source,
                } => {
                    if tty {
                        eprint!("\r\x1b[K");
                    }
                    let label = name.unwrap_or_else(|| format!("file {object:08x}"));
                    eprintln!(
                        "gave up on {label}: the sender stopped after we had {received} of the ~{source} packets needed"
                    );
                }
                Event::Rejected { from, reason } => {
                    eprintln!("ignoring packets from {}: {reason}", from.ip())
                }
                Event::Warning { message } => eprintln!("warning: {message}"),
            }
            true
        },
    )?;
    match failure {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
