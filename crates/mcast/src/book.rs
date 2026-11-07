//! `mcast book`: the phonebook of named multicast groups, and resolving
//! names given in place of a group on the command line.

use anyhow::{bail, Result};
use clap::Subcommand;
use mcast_control::phonebook::{Entry, Phonebook};
use std::net::SocketAddr;

#[derive(Subcommand)]
#[command(after_help = "Examples:
  mcast book add zone-1 239.192.2.1:10000 -d \"Ground floor paging (G.711)\"
  mcast book add \"Lobby music\" 239.69.0.31:5004
  mcast book                                    # list everything
  mcast book list zone                          # entries whose name, group or description has \"zone\"
  mcast book edit zone-1 -g 239.192.2.11:10000  # change the group, the description (-d) or the name (--rename)
  mcast book remove zone-1
  mcast audio send -g zone-1 --file page.wav    # a name works anywhere a group does
  mcast monitor zone-1 \"lobby music\"

The phonebook is kept in ~/.config/mcast/phonebook.conf ($MCAST_PHONEBOOK
overrides) and can be edited by hand. `mcast serve` shares it with the web GUI
and the API. Names match without regard to case.")]
pub enum BookCommand {
    /// List entries, optionally only those matching some text
    #[command(visible_alias = "ls")]
    List {
        /// Show entries whose name, group or description contains this
        filter: Option<String>,
    },
    /// Add a group under a name
    Add {
        /// Letters, digits, spaces, '-', '_' and '.'
        name: String,
        /// Multicast group and port, e.g. 239.192.2.1:10000 or [ff15::69]:5004
        group: SocketAddr,
        /// What it's for, e.g. "Ground floor speakers (G.711)"
        #[arg(short, long, default_value = "")]
        description: String,
        /// Replace an entry with the same name
        #[arg(short, long)]
        force: bool,
    },
    /// Change an entry's group, description or name
    Edit {
        name: String,
        /// New group and port
        #[arg(short, long)]
        group: Option<SocketAddr>,
        /// New description ("" to clear it)
        #[arg(short, long)]
        description: Option<String>,
        /// New name
        #[arg(long, value_name = "NEW_NAME")]
        rename: Option<String>,
    },
    /// Remove an entry
    #[command(visible_aliases = ["rm", "delete"])]
    Remove { name: String },
}

pub fn run(c: Option<BookCommand>) -> Result<()> {
    let mut book = Phonebook::load()?;
    match c.unwrap_or(BookCommand::List { filter: None }) {
        BookCommand::List { filter } => {
            let shown: Vec<&Entry> = book
                .entries
                .iter()
                .filter(|e| filter.as_deref().map_or(true, |f| matches(e, f)))
                .collect();
            if shown.is_empty() {
                if book.entries.is_empty() {
                    eprintln!(
                        "the phonebook ({}) is empty; add a group with `mcast book add NAME ADDR:PORT -d DESCRIPTION`",
                        book.path.display()
                    );
                } else {
                    eprintln!("nothing in the phonebook matches");
                }
                return Ok(());
            }
            print!("{}", table(&shown));
        }
        BookCommand::Add {
            name,
            group,
            description,
            force,
        } => {
            if !force {
                if let Some(e) = book.get(&name) {
                    bail!(
                        "\"{}\" is already in the phonebook ({}); use `mcast book edit`, or add --force to replace it",
                        e.name,
                        e.group
                    );
                }
            }
            if let Some(e) = book.name_of(group) {
                if !e.name.eq_ignore_ascii_case(&name) {
                    eprintln!("note: {group} is also in the phonebook as \"{}\"", e.name);
                }
            }
            let entry = Entry {
                name,
                group,
                description: description.trim().to_string(),
            };
            let new = book.put(entry.clone())?;
            book.save()?;
            println!(
                "{} {}",
                if new { "added" } else { "replaced" },
                describe(&entry)
            );
        }
        BookCommand::Edit {
            name,
            group,
            description,
            rename,
        } => {
            if group.is_none() && description.is_none() && rename.is_none() {
                bail!("nothing to change: give --group, --description or --rename");
            }
            let mut name = name;
            if let Some(to) = rename {
                name = book.rename(&name, &to)?.name.clone();
            }
            let Some(mut entry) = book.get(&name).cloned() else {
                bail!("\"{name}\" isn't in the phonebook (`mcast book` lists it)");
            };
            if let Some(g) = group {
                entry.group = g;
            }
            if let Some(d) = description {
                entry.description = d.trim().to_string();
            }
            book.put(entry.clone())?;
            book.save()?;
            println!("updated {}", describe(&entry));
        }
        BookCommand::Remove { name } => {
            let e = book.remove(&name)?;
            book.save()?;
            println!("removed {}", describe(&e));
        }
    }
    Ok(())
}

fn matches(e: &Entry, filter: &str) -> bool {
    let f = filter.to_lowercase();
    e.name.to_lowercase().contains(&f)
        || e.group.to_string().contains(&f)
        || e.description.to_lowercase().contains(&f)
}

fn describe(e: &Entry) -> String {
    if e.description.is_empty() {
        format!("{}: {}", e.name, e.group)
    } else {
        format!("{}: {} ({})", e.name, e.group, e.description)
    }
}

fn table(entries: &[&Entry]) -> String {
    let w_name = entries
        .iter()
        .map(|e| e.name.chars().count())
        .max()
        .unwrap_or(0)
        .max(4);
    let w_group = entries
        .iter()
        .map(|e| e.group.to_string().len())
        .max()
        .unwrap_or(0)
        .max(5);
    let mut s = format!("{:<w_name$}  {:<w_group$}  DESCRIPTION\n", "NAME", "GROUP");
    for e in entries {
        let line = format!(
            "{:<w_name$}  {:<w_group$}  {}",
            e.name,
            e.group.to_string(),
            e.description
        );
        s += line.trim_end();
        s.push('\n');
    }
    s
}

/// clap value parser for groups: `ADDR:PORT`, or a name from the phonebook.
pub fn parse_group(s: &str) -> Result<SocketAddr, String> {
    if let Ok(a) = s.trim().parse() {
        return Ok(a);
    }
    Phonebook::load()
        .and_then(|b| b.resolve(s))
        .map_err(|e| format!("{e:#}"))
}

/// The phonebook, if there is one: for labelling groups. Problems with it
/// are not worth failing over here.
pub fn load_quietly() -> Option<Phonebook> {
    Phonebook::load().ok().filter(|b| !b.entries.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_lines_up() {
        let a = Entry {
            name: "zone-1".into(),
            group: "239.192.2.1:10000".parse().unwrap(),
            description: "Ground floor".into(),
        };
        let b = Entry {
            name: "Lobby music".into(),
            group: "239.69.0.31:5004".parse().unwrap(),
            description: String::new(),
        };
        assert_eq!(
            table(&[&a, &b]),
            "NAME         GROUP              DESCRIPTION\n\
             zone-1       239.192.2.1:10000  Ground floor\n\
             Lobby music  239.69.0.31:5004\n"
        );
        assert!(matches(&a, "GROUND"));
        assert!(matches(&a, "192.2"));
        assert!(!matches(&b, "zone"));
    }
}
