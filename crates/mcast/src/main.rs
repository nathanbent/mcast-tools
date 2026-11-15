//! The `mcast` command. Each family of commands has its module: the
//! options clap parses and the function that runs them. What several
//! families share (network, format, SAP and RTCP options) is here.

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use mcast_audio::Codec;
use mcast_control::request::hostname;
use mcast_core::sap::{self, Directory};
use mcast_core::Interface;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

mod audio;
mod book;
mod chimes;
mod file;
mod intercom;
mod monitor;
mod presets;
mod serve;
mod text;
mod update;

use audio::AudioCommand;

#[derive(Parser)]
#[command(name = "mcast", version = env!("MCAST_VERSION"), about = "Multicast tools")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

// Parsed once at startup; variant size doesn't matter.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Command {
    /// Audio over RTP multicast: send, receive, discover
    #[command(subcommand)]
    Audio(AudioCommand),
    /// Real-time text (T.140) over RTP multicast: send, receive, chat
    #[command(subcommand)]
    Text(text::TextCommand),
    /// Files and pictures over multicast (RaptorQ fountain code): send, receive, show
    #[command(subcommand)]
    File(file::FileCommand),
    /// Watch multicast streams: rate, loss, jitter, TTL, DSCP and audio level
    Monitor(monitor::MonitorArgs),
    /// Phonebook: save multicast groups under a name and description; use the name in place of the group
    Book {
        #[command(subcommand)]
        command: Option<book::BookCommand>,
    },
    /// Save options for `audio send`, `recv`, `discover`, `monitor` or `update` under a name; run with `mcast @NAME`
    #[command(subcommand)]
    Preset(presets::PresetCommand),
    /// Update mcast: download the latest release, or rebuild it with cargo
    Update(update::UpdateArgs),
    /// Run the web GUI and HTTP/JSON API for sending, receiving, discovery and monitoring
    Serve(serve::ServeArgs),
    /// Terminal front end for a running `mcast serve` (works over SSH)
    Tui(serve::TuiArgs),
    /// Intercom: play announcements to zones of speakers through a running `mcast serve`
    Intercom(intercom::IntercomArgs),
}

#[derive(Args)]
struct NetArgs {
    /// Interface: IPv4 address (IPv4 groups) or interface index (IPv6 groups)
    #[arg(short, long, default_value = "any")]
    iface: Interface,
    /// Multicast TTL / hop limit
    #[arg(long, default_value_t = 16)]
    ttl: u32,
    /// Don't loop packets back to receivers on this host
    #[arg(long)]
    no_loopback: bool,
}

impl NetArgs {
    /// Socket options for `group` (text and file commands; audio goes
    /// through `SendRequest`/`RecvRequest`, which do the same).
    fn options(&self, group: SocketAddr) -> Result<mcast_core::MulticastOptions> {
        mcast_control::request::NetRequest {
            interface: Some(self.iface),
            ttl: self.ttl,
            loopback: !self.no_loopback,
        }
        .options(group, Interface::Any)
    }
}

#[derive(Args)]
struct FormatArgs {
    /// Sample rate in Hz [default: 48000]
    #[arg(short, long)]
    rate: Option<u32>,
    /// Channel count [default: 2]
    #[arg(short, long)]
    channels: Option<u16>,
    /// Encoding: l16, l24, pcmu/g711u, pcma/g711a, g722, opus [default: l16]
    #[arg(short, long)]
    encoding: Option<Codec>,
}

#[derive(Args)]
struct SapListenArgs {
    /// SAP group(s) to listen on [default: 239.255.255.255, 239.195.255.255, 224.2.127.254 port 9875]
    #[arg(long = "sap-group", value_name = "ADDR:PORT")]
    sap_groups: Vec<SocketAddr>,
}

impl SapListenArgs {
    fn open(&self, iface: Interface) -> Result<Directory> {
        let groups = if self.sap_groups.is_empty() {
            sap::default_listen_groups()
        } else {
            self.sap_groups.clone()
        };
        // Forget sessions that stop announcing (senders re-announce every few seconds to ~minutes).
        Ok(Directory::open(&groups, iface, Duration::from_secs(300))?)
    }
}

#[derive(Args)]
struct RtcpArgs {
    /// Don't send or listen for RTCP (sender/receiver reports on the RTP port + 1)
    #[arg(long)]
    no_rtcp: bool,
    /// Average seconds between RTCP reports
    #[arg(long, default_value_t = 5.0, value_name = "SECS")]
    rtcp_interval: f64,
}

impl RtcpArgs {
    fn interval(&self) -> Option<Duration> {
        (!self.no_rtcp).then(|| Duration::from_secs_f64(self.rtcp_interval.max(0.1)))
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse_from(presets::expand(std::env::args_os().collect())?);
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed))?;
    }
    match cli.command {
        Command::Audio(c) => audio::run(c, &stop),
        Command::Text(c) => text::run(c, &stop),
        Command::File(c) => file::run(c, &stop),
        Command::Monitor(a) => monitor::run(a, &stop),
        Command::Book { command } => book::run(command),
        Command::Preset(c) => presets::run(c),
        Command::Update(a) => update::run(a),
        Command::Serve(a) => serve::run(a, stop),
        Command::Tui(a) => serve::tui(a),
        Command::Intercom(a) => intercom::run(a, stop),
    }
}
