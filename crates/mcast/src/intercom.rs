//! `mcast intercom`: the intercom panel and its command line.

use crate::serve::shutdown_signal;
use anyhow::Result;
use clap::{Args, Subcommand};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[derive(Args)]
#[command(
    after_help = "Zones, announcements and schedules are kept in intercom.json in the mcast \
config directory (or $MCAST_INTERCOM); see crates/mcast-intercom/README.md for its format.

Examples:
  mcast intercom serve                  # the panel and schedules: http://127.0.0.1:8651/
  MCAST_INTERCOM_TOKEN=change-me mcast intercom serve --listen 0.0.0.0:8651
  mcast intercom list
  mcast intercom check
  mcast intercom upcoming               # what the schedules play next
  mcast intercom play lunch             # by id or name; Ctrl-C stops it
  mcast intercom token add Doorbell --role trigger --announcement doorbell
  mcast intercom token list
  mcast intercom backup --sounds        # intercom-DATE.tar.gz: the config and every sound
  mcast intercom restore intercom-2026-11-08.tar.gz --dry-run"
)]
pub(crate) struct IntercomArgs {
    #[command(subcommand)]
    command: IntercomCommand,
    /// The mcast server to send through
    #[arg(
        short,
        long,
        global = true,
        default_value = "http://127.0.0.1:8650",
        env = "MCAST_URL"
    )]
    url: String,
    /// Its API token
    #[arg(long, global = true, env = "MCAST_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// The intercom config file
    #[arg(long, global = true, value_name = "FILE", env = "MCAST_INTERCOM")]
    config: Option<PathBuf>,
    /// The access tokens' file (default: tokens.json beside the config)
    #[arg(
        long,
        global = true,
        value_name = "FILE",
        env = "MCAST_INTERCOM_TOKENS"
    )]
    tokens: Option<PathBuf>,
}

#[derive(Subcommand)]
pub(crate) enum IntercomCommand {
    /// List zones and announcements
    List,
    /// Check the config, and that its media files and phonebook names exist on the server
    Check,
    /// Play an announcement (by id or name) and follow it until it ends
    Play { announcement: String },
    /// The next times anything is scheduled to play
    Upcoming {
        /// How many
        #[arg(short, default_value_t = 10)]
        n: usize,
    },
    /// Run the intercom panel (a web page and HTTP/JSON API of its own) and its schedules
    Serve(IntercomServeArgs),
    /// Access tokens for the panel: named keys, each with a role
    #[command(subcommand)]
    Token(TokenCommand),
    /// Save the config (and with --sounds, every sound on the mcast server) to a file.
    /// Access tokens, webhooks and the play history are never in it
    Backup {
        /// Include the sounds: a .tar.gz instead of an intercom.json
        #[arg(long)]
        sounds: bool,
        /// Where to write it (default: intercom-DATE.json or .tar.gz here)
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,
    },
    /// Put a backup back: upload its sounds the mcast server lacks, then replace the
    /// config (the old one is kept as intercom.json.bak). A running panel picks it up
    Restore {
        /// A .tar.gz or intercom.json from `mcast intercom backup` or the panel
        file: PathBuf,
        /// Only say what would change
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum TokenCommand {
    /// Make a token and print its secret (shown only this once)
    #[command(after_help = "Roles:
  admin     everything, tokens included
  operator  use the panel: play, stop, push to talk, history; change nothing
  trigger   other systems: play announcements (--announcement limits which) and stop them
  media     sound files: list, upload, download, delete, folders")]
    Add {
        /// What it's for, e.g. "Front doorbell"
        name: String,
        /// admin, operator, trigger or media
        #[arg(long)]
        role: mcast_intercom::tokens::Role,
        /// Trigger tokens: an announcement (id or name) it may play; repeat for more. None: any
        #[arg(long = "announcement", value_name = "ANNOUNCEMENT")]
        announcements: Vec<String>,
        /// Operator tokens: a category (id or name) whose announcements it sees and
        /// plays; repeat for more. None: all
        #[arg(long = "category", value_name = "CATEGORY")]
        categories: Vec<String>,
        /// The last day it works (YYYY-MM-DD, in the panel's time zone)
        #[arg(long, value_name = "DATE")]
        expires: Option<String>,
    },
    /// List the tokens (never their secrets)
    List,
    /// Revoke a token, by id or name; it stops working at once
    Revoke { token: String },
}

#[derive(Args)]
pub(crate) struct IntercomServeArgs {
    /// Address to listen on
    #[arg(short, long, default_value = "127.0.0.1:8651")]
    listen: SocketAddr,
    /// The startup token: an admin key to the panel (needed to listen beyond
    /// loopback unless tokens.json has an admin token); --token is the mcast server's
    #[arg(long, env = "MCAST_INTERCOM_TOKEN", hide_env_values = true)]
    panel_token: Option<String>,
    /// API only: don't serve the web page
    #[arg(long)]
    no_web: bool,
    /// Serve the web page from this directory instead of the built-in copy
    /// (for working on crates/mcast-intercom/assets)
    #[arg(long, value_name = "DIR", conflicts_with = "no_web")]
    web_dir: Option<PathBuf>,
    /// Serve HTTPS with this certificate (PEM, with its chain); needs --tls-key.
    /// Browsers only allow push to talk's microphone on HTTPS pages (or localhost)
    #[arg(
        long,
        value_name = "FILE",
        env = "MCAST_INTERCOM_TLS_CERT",
        requires = "tls_key"
    )]
    tls_cert: Option<PathBuf>,
    /// The certificate's private key (PEM)
    #[arg(
        long,
        value_name = "FILE",
        env = "MCAST_INTERCOM_TLS_KEY",
        requires = "tls_cert"
    )]
    tls_key: Option<PathBuf>,
    /// Serve HTTPS with a self-signed certificate, made once and kept beside the
    /// config (browsers warn about it the first time). The default when
    /// listening on anything but loopback; --tls-cert wins over it
    #[arg(long, env = "MCAST_INTERCOM_TLS_SELF_SIGNED")]
    tls_self_signed: bool,
    /// Another name or address for the self-signed certificate (repeatable)
    #[arg(long = "tls-name", value_name = "NAME")]
    tls_names: Vec<String>,
    /// Plain HTTP, even when listening on the network (push to talk then only
    /// works from http://localhost, or behind a proxy that adds HTTPS)
    #[arg(long, env = "MCAST_INTERCOM_NO_TLS", conflicts_with_all = ["tls_cert", "tls_self_signed"])]
    no_tls: bool,
}

pub(crate) fn run(a: IntercomArgs, stop: Arc<AtomicBool>) -> Result<()> {
    let path = match a.config {
        Some(p) => p,
        None => mcast_intercom::store::default_path()?,
    };
    let tokens = a
        .tokens
        .clone()
        .unwrap_or_else(|| mcast_intercom::tokens::default_path(&path));
    let backend = mcast_intercom::HttpBackend::new(&a.url, a.token);
    match a.command {
        IntercomCommand::Token(t) => match t {
            TokenCommand::Add {
                name,
                role,
                announcements,
                categories,
                expires,
            } => mcast_intercom::cli::token_add(
                &path,
                &tokens,
                mcast_intercom::tokens::NewToken {
                    name,
                    role,
                    announcements,
                    categories,
                    expires,
                },
            ),
            TokenCommand::List => mcast_intercom::cli::token_list(&path, &tokens),
            TokenCommand::Revoke { token } => mcast_intercom::cli::token_revoke(&tokens, &token),
        },
        IntercomCommand::List => mcast_intercom::cli::list(&path),
        IntercomCommand::Upcoming { n } => mcast_intercom::cli::upcoming(&path, n),
        IntercomCommand::Check => mcast_intercom::cli::check(&path, &backend),
        IntercomCommand::Backup { sounds, output } => {
            mcast_intercom::cli::backup(&path, &backend, sounds, output)
        }
        IntercomCommand::Restore { file, dry_run } => {
            mcast_intercom::cli::restore(&path, &backend, &file, dry_run)
        }
        IntercomCommand::Play { announcement } => {
            mcast_intercom::cli::play(&path, Arc::new(backend), &announcement, &stop)
        }
        IntercomCommand::Serve(s) => {
            use mcast_intercom::server::tls;
            // HTTPS unless asked not to, or only reachable from this machine:
            // browsers only allow the microphone on HTTPS pages (and localhost).
            let self_signed = s.tls_self_signed || (!s.no_tls && !s.listen.ip().is_loopback());
            let tls = match (s.tls_cert, s.tls_key) {
                (Some(cert), Some(key)) => Some(tls::TlsFiles { cert, key }),
                _ if self_signed => {
                    let dir = path.parent().unwrap_or(std::path::Path::new("."));
                    Some(tls::self_signed(dir, s.listen, &s.tls_names)?)
                }
                _ => None,
            };
            let cfg = mcast_intercom::server::ServeConfig {
                tls,
                listen: s.listen,
                token: s.panel_token,
                config: path,
                tokens: Some(tokens),
                web_dir: s.web_dir,
                web: !s.no_web,
            };
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(mcast_intercom::server::serve(
                cfg,
                Arc::new(backend),
                shutdown_signal(stop),
            ))
        }
    }
}
