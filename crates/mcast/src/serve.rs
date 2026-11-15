//! `mcast serve` (the web GUI and HTTP API) and `mcast tui`.

use crate::SapListenArgs;
use anyhow::Result;
use clap::Args;
use mcast_core::sap;
use mcast_core::Interface;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// A voice in --tts-voice-list (blank ones are skipped).
fn parse_voice_name(s: &str) -> Result<String, String> {
    let v = s.trim();
    if v.is_empty() || mcast_audio::tts::is_voice_name(v) {
        Ok(v.to_string())
    } else {
        Err("a voice is letters, digits and -_.+ (up to 100)".into())
    }
}

#[derive(Args)]
pub(crate) struct ServeArgs {
    /// Address to listen on. Anything but loopback needs --token
    #[arg(short, long, env = "MCAST_LISTEN", default_value = "127.0.0.1:8650")]
    listen: SocketAddr,
    /// Bearer token clients must send (Authorization: Bearer TOKEN)
    #[arg(long, env = "MCAST_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Directory for uploaded audio [default: ~/.local/share/mcast/media]
    #[arg(long, value_name = "DIR")]
    media_dir: Option<PathBuf>,
    /// Largest upload in MB
    #[arg(long, default_value_t = 200)]
    max_upload_mb: u64,
    /// Interface for jobs, discovery and monitoring: IPv4 address or interface index
    #[arg(short, long, default_value = "any")]
    iface: Interface,
    #[command(flatten)]
    sap: SapListenArgs,
    /// Also monitor these groups (ADDR:PORT or phonebook names), besides SAP-announced streams
    #[arg(long = "watch", value_name = "GROUP", value_parser = crate::book::parse_group)]
    watch: Vec<SocketAddr>,
    /// Don't run the stream monitor
    #[arg(long, conflicts_with = "watch")]
    no_monitor: bool,
    /// API only: don't serve the web GUI
    #[arg(long)]
    no_web: bool,
    /// Serve the web GUI from this directory instead of the built-in copy
    /// (for working on crates/mcast-web/assets)
    #[arg(long, value_name = "DIR", conflicts_with = "no_web")]
    web_dir: Option<PathBuf>,
    /// Text to speech: Piper voices (NAME.onnx with NAME.onnx.json)
    /// [default: /usr/local/share/mcast/voices]. Without Piper or a voice,
    /// eSpeak NG is used if installed
    #[arg(long, value_name = "DIR", env = "MCAST_TTS_VOICES")]
    tts_voices: Option<PathBuf>,
    /// Text to speech: the voice used when a request names none
    #[arg(long, value_name = "NAME", env = "MCAST_TTS_VOICE")]
    tts_voice: Option<String>,
    /// Text to speech by this command instead (run with sh -c): text on
    /// stdin, a WAV on stdout, the voice asked for in $MCAST_TTS_VOICE
    #[arg(long, value_name = "COMMAND", env = "MCAST_TTS_COMMAND")]
    tts_command: Option<String>,
    /// Text to speech: the voices --tts-command offers, comma-separated, so
    /// clients can list them (GET /voices)
    #[arg(
        long,
        value_name = "NAMES",
        env = "MCAST_TTS_VOICE_LIST",
        value_delimiter = ',',
        value_parser = parse_voice_name
    )]
    tts_voice_list: Vec<String>,
    /// Text to speech: seconds the engine may take to speak each item
    /// before the page fails [default: 60]
    #[arg(
        long,
        value_name = "SECS",
        env = "MCAST_TTS_TIMEOUT",
        value_parser = clap::value_parser!(u64).range(1..=3600)
    )]
    tts_timeout: Option<u64>,
    /// Where spoken text is kept, so it's synthesized once [default: under the temporary directory]
    #[arg(long, value_name = "DIR", env = "MCAST_TTS_CACHE")]
    tts_cache: Option<PathBuf>,
}

#[derive(Args)]
pub(crate) struct TuiArgs {
    /// The server to control
    #[arg(
        short,
        long,
        default_value = "http://127.0.0.1:8650",
        env = "MCAST_URL"
    )]
    url: String,
    /// Its API token
    #[arg(long, env = "MCAST_TOKEN", hide_env_values = true)]
    token: Option<String>,
}

pub(crate) fn tui(a: TuiArgs) -> Result<()> {
    mcast_tui::run(&a.url, a.token)
}

pub(crate) fn run(a: ServeArgs, stop: Arc<AtomicBool>) -> Result<()> {
    let media_dir = a.media_dir.unwrap_or_else(|| {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("mcast/media")
    });
    let cfg = mcast_server::ServerConfig {
        listen: a.listen,
        token: a.token,
        media_dir,
        interface: a.iface,
        sap_groups: if a.sap.sap_groups.is_empty() {
            sap::default_listen_groups()
        } else {
            a.sap.sap_groups
        },
        monitor: (!a.no_monitor).then_some(a.watch),
        phonebook: mcast_control::phonebook::default_path()?,
        chimes: mcast_control::chimes::default_path()?,
        max_upload: a.max_upload_mb.saturating_mul(1 << 20),
        tts: mcast_audio::tts::TtsConfig {
            command: a.tts_command,
            voices: a.tts_voices,
            voice: a.tts_voice,
            command_voices: a.tts_voice_list,
            timeout: a.tts_timeout.map(std::time::Duration::from_secs),
            cache: a.tts_cache,
        },
    };
    let web = if a.no_web {
        axum::Router::new()
    } else {
        mcast_web::router(a.web_dir)
    };
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(mcast_server::serve(cfg, web, shutdown_signal(stop)))
}

/// SIGTERM (systemd, docker stop) or Ctrl-C, which the ctrlc handler turns into `stop`.
pub(crate) async fn shutdown_signal(stop: Arc<AtomicBool>) {
    let sigterm = async {
        #[cfg(unix)]
        if let Ok(mut t) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            t.recv().await;
            return;
        }
        std::future::pending::<()>().await
    };
    let ctrl_c = async {
        while !stop.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    };
    tokio::select! {
        _ = sigterm => {}
        _ = ctrl_c => {}
    }
}
