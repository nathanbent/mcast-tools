//! `mcast audio chimes`: list the built-in chimes, hide the ones not wanted
//! from the GUI's lists (and show them again), and save one as a WAV file.

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use mcast_audio::chime::{BuiltIn, Category};
use mcast_control::chimes::{default_path, ChimeSettings};
use std::path::PathBuf;

#[derive(Subcommand)]
#[command(after_help = "Examples:
  mcast audio chimes                       # the chimes the lists offer
  mcast audio chimes list --all            # every built-in, hidden ones too
  mcast audio chimes hide klaxon cowbell   # leave these out of the lists
  mcast audio chimes show --all            # offer every chime again
  mcast audio chimes save church-bell bell.wav

Hidden chimes are left out of the lists in the web GUI, mcast tui and the
intercom panel, but -C NAME and requests that name one still play it. Which
are hidden is kept in chimes.json in the mcast config directory
($MCAST_CHIMES overrides), shared with mcast serve.")]
pub enum ChimesCommand {
    /// List the built-in chimes (the default)
    List {
        /// Hidden ones too
        #[arg(short, long)]
        all: bool,
    },
    /// Leave chimes out of the lists
    Hide {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Offer hidden chimes in the lists again
    Show {
        #[arg(required_unless_present = "all")]
        names: Vec<String>,
        /// Every chime
        #[arg(short, long, conflicts_with = "names")]
        all: bool,
    },
    /// Save a built-in chime as a WAV file, to listen to it
    Save { name: String, file: PathBuf },
}

fn parse(names: &[String]) -> Result<Vec<BuiltIn>> {
    names
        .iter()
        .map(|n| n.parse::<BuiltIn>().map_err(anyhow::Error::msg))
        .collect()
}

fn joined(chimes: &[BuiltIn]) -> String {
    chimes
        .iter()
        .map(|b| b.name())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn run(command: Option<ChimesCommand>) -> Result<()> {
    let path = default_path()?;
    let mut settings = ChimeSettings::load_from(&path)?;
    match command.unwrap_or(ChimesCommand::List { all: false }) {
        ChimesCommand::List { all } => {
            let width = BuiltIn::ALL
                .iter()
                .map(|b| b.name().len())
                .max()
                .unwrap_or(0);
            for cat in Category::ALL {
                let chimes: Vec<_> = BuiltIn::ALL
                    .iter()
                    .filter(|b| b.category() == cat && (all || !settings.is_hidden(**b)))
                    .collect();
                if chimes.is_empty() {
                    continue;
                }
                println!("{}:", cat.label());
                for b in chimes {
                    let hidden = if settings.is_hidden(*b) {
                        " (hidden)"
                    } else {
                        ""
                    };
                    println!("  {:width$}  {}{hidden}", b.name(), b.description());
                }
            }
            let hidden = BuiltIn::ALL
                .iter()
                .filter(|b| settings.is_hidden(**b))
                .count();
            if hidden > 0 && !all {
                println!("({hidden} hidden; list --all to see them)");
            }
        }
        ChimesCommand::Hide { names } => {
            let chimes = parse(&names)?;
            for &b in &chimes {
                settings.set_hidden(b, true);
            }
            settings.save_to(&path)?;
            println!("Hidden: {}", joined(&chimes));
        }
        ChimesCommand::Show { names, all } => {
            if all {
                settings.hidden.clear();
            } else {
                for b in parse(&names)? {
                    settings.set_hidden(b, false);
                }
            }
            settings.save_to(&path)?;
            println!(
                "Showing {}",
                if all {
                    "every chime".into()
                } else {
                    names.join(", ")
                }
            );
        }
        ChimesCommand::Save { name, file } => {
            let b: BuiltIn = name.parse().map_err(anyhow::Error::msg)?;
            if file.exists() {
                bail!("{} already exists", file.display());
            }
            let wav = mcast_audio::chime::preview_wav(b)?;
            std::fs::write(&file, wav).with_context(|| format!("writing {}", file.display()))?;
            println!("Saved {b} to {}", file.display());
        }
    }
    Ok(())
}
