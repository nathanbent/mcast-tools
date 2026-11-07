//! Named presets: saved options for `mcast audio send` and `mcast audio recv`.
//!
//! A preset is stored as the options it adds, one per line, in a small text
//! file (`~/.config/mcast/presets.conf`, or `$MCAST_PRESETS`):
//!
//! ```text
//! [page-zone-1]
//! audio send
//! --group=239.192.2.1:10000
//! --encoding=g711u
//! --voice
//! ```
//!
//! Using one (`mcast @page-zone-1 --file page.wav`, or `--preset` on send/recv)
//! rewrites the command line before clap parses it: the preset's options go
//! first, minus any that the command line sets again or conflicts with, so
//! options typed on the command line always win.

use crate::{RecvArgs, SendArgs};
use anyhow::{anyhow, bail, Context, Result};
use clap::parser::ValueSource;
use clap::{Arg, Args, Command};
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

/// The command a preset runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Send,
    Recv,
}

impl Kind {
    fn sub(self) -> &'static str {
        match self {
            Kind::Send => "send",
            Kind::Recv => "recv",
        }
    }

    fn parse(s: &str) -> Option<Kind> {
        match s.split_whitespace().collect::<Vec<_>>()[..] {
            ["audio", "send"] | ["send"] => Some(Kind::Send),
            ["audio", "recv"] | ["recv"] => Some(Kind::Recv),
            _ => None,
        }
    }

    /// The clap command for this kind's options (built, so groups exist).
    fn command(self) -> Command {
        let name = match self {
            Kind::Send => "mcast audio send",
            Kind::Recv => "mcast audio recv",
        };
        let c = Command::new(name).no_binary_name(true);
        let mut c = match self {
            Kind::Send => SendArgs::augment_args(c),
            Kind::Recv => RecvArgs::augment_args(c),
        };
        c.build();
        c
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "audio {}", self.sub())
    }
}

/// One option of a preset, in canonical `--long[=value]` form.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    id: String,
    tokens: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Preset {
    pub name: String,
    pub kind: Kind,
    /// Option lines, as in the file (canonical `--long=value` once changed).
    pub options: Vec<String>,
}

impl Preset {
    /// The full command this preset runs, shell-quoted.
    pub fn command_line(&self) -> String {
        let mut s = format!("mcast {}", self.kind);
        for t in self.tokens() {
            s.push(' ');
            s += &shell_quote(&t);
        }
        s
    }

    fn entries(&self) -> Result<Vec<Entry>> {
        normalize(self.kind, &self.tokens()).map_err(|e| anyhow!("preset \"{}\": {e}", self.name))
    }

    /// Command-line tokens: a line like `-g 239.1.1.1:5004` is two.
    fn tokens(&self) -> Vec<String> {
        self.options.iter().flat_map(|l| line_tokens(l)).collect()
    }

    fn set_entries(&mut self, entries: Vec<Entry>) {
        self.options = entries.into_iter().flat_map(|e| e.tokens).collect();
    }
}

fn shell_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_=.,:/@+%[]".contains(c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Clap errors read "error: ..."; anyhow adds its own prefix.
fn clap_message(e: clap::Error) -> String {
    let s = e.to_string();
    s.trim_start_matches("error: ").trim_end().to_string()
}

/// Parse option tokens with the kind's real parser (so values are validated)
/// and turn each option given into canonical tokens, in command-line order.
fn normalize<S: AsRef<str>>(kind: Kind, tokens: &[S]) -> Result<Vec<Entry>, String> {
    let cmd = kind.command();
    let m = cmd
        .clone()
        .try_get_matches_from(tokens.iter().map(|t| t.as_ref()))
        .map_err(|e| {
            if matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                e.exit();
            }
            clap_message(e)
        })?;
    let mut out = Vec::new();
    for arg in cmd.get_arguments() {
        let id = arg.get_id().as_str();
        if id == "preset" || m.value_source(id) != Some(ValueSource::CommandLine) {
            continue;
        }
        let Some(long) = arg.get_long() else {
            continue; // help/version, or a positional (send/recv have none)
        };
        let tokens = if arg.get_action().takes_values() {
            m.get_raw(id)
                .into_iter()
                .flatten()
                .map(|v| format!("--{long}={}", v.to_string_lossy()))
                .collect()
        } else {
            vec![format!("--{long}")]
        };
        out.push((
            m.index_of(id).unwrap_or(usize::MAX),
            Entry {
                id: id.into(),
                tokens,
            },
        ));
    }
    out.sort_by_key(|(i, _)| *i);
    Ok(out.into_iter().map(|(_, e)| e).collect())
}

fn find_arg<'a>(cmd: &'a Command, id: &str) -> Option<&'a Arg> {
    cmd.get_arguments().find(|a| a.get_id() == id)
}

/// Whether options `a` and `b` can't both be kept: the same option, declared
/// conflicts (--voice / --mode), or members of one exclusive group (--tone / --file).
fn conflicts(cmd: &Command, a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (Some(x), Some(y)) = (find_arg(cmd, a), find_arg(cmd, b)) else {
        return false;
    };
    let declared = |p: &Arg, q: &str| {
        cmd.get_arg_conflicts_with(p)
            .iter()
            .any(|c| c.get_id() == q)
    };
    if declared(x, b) || declared(y, a) {
        return true;
    }
    cmd.get_groups().any(|g| {
        let members: Vec<_> = g.get_args().map(|i| i.as_str()).collect();
        !g.clone().is_multiple() && members.contains(&a) && members.contains(&b)
    })
}

/// `base` with `over` applied: options in `over` replace the same or
/// conflicting options in `base`.
fn merge(cmd: &Command, mut base: Vec<Entry>, over: Vec<Entry>) -> Vec<Entry> {
    base.retain(|e| !over.iter().any(|o| conflicts(cmd, &e.id, &o.id)));
    base.extend(over);
    base
}

/// Find the options on a command line without validating it (it may only be
/// valid once a preset fills in the rest). Returns their ids, and the values
/// of any --preset options.
fn scan(cmd: &Command, tokens: &[OsString]) -> (Vec<String>, Vec<String>) {
    let mut ids = Vec::new();
    let mut presets = Vec::new();
    let mut i = 0;
    let next_value = |i: usize, optional: bool| -> Option<String> {
        let v = tokens.get(i + 1)?.to_string_lossy().into_owned();
        (!optional || !v.starts_with('-')).then_some(v)
    };
    while i < tokens.len() {
        let t = tokens[i].to_string_lossy().into_owned();
        if t == "--" {
            break;
        }
        // (arg, value given inline)
        let mut found: Option<(&Arg, Option<String>)> = None;
        if let Some(rest) = t.strip_prefix("--") {
            let (name, inline) = match rest.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (rest, None),
            };
            found = cmd
                .get_arguments()
                .find(|a| {
                    a.get_long() == Some(name)
                        || a.get_all_aliases().is_some_and(|al| al.contains(&name))
                })
                .map(|a| (a, inline));
        } else if let Some(rest) = t.strip_prefix('-') {
            // Bundled shorts (-pC): flags until one that takes a value.
            for (j, c) in rest.char_indices() {
                let Some(a) = cmd.get_arguments().find(|a| {
                    a.get_short() == Some(c)
                        || a.get_all_short_aliases().is_some_and(|al| al.contains(&c))
                }) else {
                    break; // not an option (e.g. a negative number)
                };
                let inline = &rest[j + c.len_utf8()..];
                if a.get_action().takes_values() {
                    found = Some((a, (!inline.is_empty()).then(|| inline.to_string())));
                    break;
                }
                ids.push(a.get_id().to_string());
            }
        }
        if let Some((a, inline)) = found {
            let mut value = inline;
            if a.get_action().takes_values() && value.is_none() {
                let optional = a.get_num_args().is_some_and(|n| n.min_values() == 0);
                value = next_value(i, optional);
                if value.is_some() {
                    i += 1;
                }
            }
            if a.get_id() == "preset" {
                presets.extend(value);
            } else {
                ids.push(a.get_id().to_string());
            }
        }
        i += 1;
    }
    (ids, presets)
}

/// Rewrite the command line if it uses presets: `mcast @NAME ...`,
/// `mcast preset run NAME ...`, or `mcast audio send|recv --preset NAME ...`.
/// Other command lines are returned unchanged.
pub fn expand(argv: Vec<OsString>) -> Result<Vec<OsString>> {
    let word = |i: usize| argv.get(i).and_then(|a| a.to_str());
    let (first, kind, rest) = if let Some(name) = word(1).and_then(|a| a.strip_prefix('@')) {
        (Some(name.to_string()), None, 2)
    } else if word(1) == Some("preset") && word(2) == Some("run") && argv.len() > 3 {
        match word(3) {
            Some(n) if !n.starts_with('-') => (Some(n.to_string()), None, 4),
            _ => return Ok(argv),
        }
    } else if word(1) == Some("audio") {
        match word(2) {
            Some("send") => (None, Some(Kind::Send), 3),
            Some("recv") => (None, Some(Kind::Recv), 3),
            _ => return Ok(argv),
        }
    } else {
        return Ok(argv);
    };
    let cli = &argv[rest..];

    let mut store = None;
    let mut load = |name: &str| -> Result<Preset> {
        if store.is_none() {
            store = Some(Store::load()?);
        }
        store.as_ref().unwrap().get(name).cloned()
    };
    let first = first.map(|n| load(&n)).transpose()?;
    let kind = kind
        .or(first.as_ref().map(|p| p.kind))
        .expect("kind or preset");
    let cmd = kind.command();
    let (cli_ids, names) = scan(&cmd, cli);
    if first.is_none() && names.is_empty() {
        return Ok(argv);
    }
    let mut presets: Vec<Preset> = first.into_iter().collect();
    for n in &names {
        presets.push(load(n)?);
    }
    let mut entries = Vec::new();
    for p in &presets {
        if p.kind != kind {
            bail!(
                "preset \"{}\" is for `mcast {}`, not `mcast {kind}`",
                p.name,
                p.kind
            );
        }
        entries = merge(&cmd, entries, p.entries()?);
    }
    entries.retain(|e| !cli_ids.iter().any(|c| conflicts(&cmd, &e.id, c)));

    let mut out: Vec<OsString> = vec![argv[0].clone(), "audio".into(), kind.sub().into()];
    out.extend(
        entries
            .into_iter()
            .flat_map(|e| e.tokens)
            .map(OsString::from),
    );
    out.extend(cli.iter().cloned());
    Ok(out)
}

/// The presets file.
pub struct Store {
    pub path: PathBuf,
    pub presets: Vec<Preset>,
}

pub fn store_path() -> Result<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(p) = var("MCAST_PRESETS") {
        return Ok(p);
    }
    let dir = if cfg!(windows) {
        var("APPDATA")
    } else {
        var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|h| h.join(".config")))
    };
    let dir = dir.context("can't find a config directory; set MCAST_PRESETS to a file path")?;
    Ok(dir.join("mcast").join("presets.conf"))
}

/// Names go in `[...]` headers and after `@` on the command line.
pub fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || "-_.".contains(c));
    if !ok {
        bail!("invalid preset name \"{name}\": use letters, digits, '-', '_' and '.', not starting with '-'");
    }
    Ok(())
}

/// One option per line. `-g 239.1.1.1:5004` and `--file my page.wav` are
/// split at the first space (option names never contain one).
fn line_tokens(line: &str) -> Vec<String> {
    match line.split_once(char::is_whitespace) {
        Some((flag, value)) if !flag.contains('=') => {
            vec![flag.to_string(), value.trim().to_string()]
        }
        _ => vec![line.to_string()],
    }
}

fn parse_file(text: &str) -> Result<Vec<Preset>> {
    let mut out: Vec<Preset> = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let name = name.trim().to_string();
            if out.iter().any(|p| p.name == name) {
                bail!("line {}: preset \"{name}\" is defined twice", n + 1);
            }
            out.push(Preset {
                name,
                kind: Kind::Send,
                options: Vec::new(),
            });
            continue;
        }
        let Some(p) = out.last_mut() else {
            bail!("line {}: expected a [name] header before \"{line}\"", n + 1);
        };
        if line.starts_with('-') {
            p.options.push(line.to_string());
        } else {
            p.kind = Kind::parse(line).with_context(|| {
                format!(
                    "line {}: expected \"audio send\", \"audio recv\" or an option, got \"{line}\"",
                    n + 1
                )
            })?;
        }
    }
    Ok(out)
}

fn format_file(presets: &[Preset]) -> String {
    let mut s = String::from(
        "# mcast presets (see `mcast preset --help`).\n\
         # Each [name] holds the command it runs and its options, one per line.\n",
    );
    for p in presets {
        s += &format!("\n[{}]\n{}\n", p.name, p.kind);
        for t in &p.options {
            s += t;
            s.push('\n');
        }
    }
    s
}

impl Store {
    pub fn load() -> Result<Store> {
        let path = store_path()?;
        let presets = match std::fs::read_to_string(&path) {
            Ok(text) => parse_file(&text).with_context(|| format!("reading {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Store { path, presets })
    }

    pub fn save(&self) -> Result<()> {
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let tmp = self.path.with_extension("conf.tmp");
        std::fs::write(&tmp, format_file(&self.presets))
            .and_then(|_| std::fs::rename(&tmp, &self.path))
            .with_context(|| format!("writing {}", self.path.display()))
    }

    pub fn get(&self, name: &str) -> Result<&Preset> {
        self.presets
            .iter()
            .find(|p| p.name == name)
            .with_context(|| format!("no preset named \"{name}\" (`mcast preset list` shows them)"))
    }

    fn get_mut(&mut self, name: &str) -> Result<&mut Preset> {
        self.presets
            .iter_mut()
            .find(|p| p.name == name)
            .with_context(|| format!("no preset named \"{name}\" (`mcast preset list` shows them)"))
    }

    pub fn create(&mut self, name: &str, kind: Kind, options: &[OsString]) -> Result<&Preset> {
        check_name(name)?;
        if self.presets.iter().any(|p| p.name == name) {
            bail!("preset \"{name}\" already exists; change it with `mcast preset set {name} ...`");
        }
        let mut p = Preset {
            name: name.into(),
            kind,
            options: Vec::new(),
        };
        p.set_entries(normalize(kind, &strings(options)?).map_err(|e| anyhow!(e))?);
        self.presets.push(p);
        Ok(self.presets.last().unwrap())
    }

    /// Add or replace options. Returns the preset and whether it was created.
    pub fn set(&mut self, name: &str, options: &[OsString]) -> Result<(&Preset, bool)> {
        let created = !self.presets.iter().any(|p| p.name == name);
        if created {
            self.create(name, Kind::Send, &[])?;
        }
        let p = self.get_mut(name)?;
        let cmd = p.kind.command();
        let new = normalize(p.kind, &strings(options)?).map_err(|e| anyhow!(e))?;
        let merged = merge(&cmd, p.entries()?, new);
        p.set_entries(merged);
        Ok((p, created))
    }

    /// Remove options named like `group`, `--group` or `-g`.
    pub fn unset(&mut self, name: &str, keys: &[String]) -> Result<&Preset> {
        let p = self.get_mut(name)?;
        let cmd = p.kind.command();
        let mut entries = p.entries()?;
        for key in keys {
            let k = key.trim_start_matches('-');
            let short = (k.chars().count() == 1).then(|| k.chars().next().unwrap());
            let arg = cmd
                .get_arguments()
                .find(|a| {
                    a.get_long() == Some(k)
                        || a.get_all_aliases().is_some_and(|al| al.contains(&k))
                        || (short.is_some() && a.get_short() == short)
                })
                .filter(|a| a.get_long().is_some() && a.get_id() != "preset")
                .with_context(|| format!("`mcast {}` has no option \"{key}\"", p.kind))?;
            let before = entries.len();
            entries.retain(|e| e.id != arg.get_id().as_str());
            if entries.len() == before {
                eprintln!(
                    "note: \"{}\" doesn't set --{}",
                    p.name,
                    arg.get_long().unwrap()
                );
            }
        }
        p.set_entries(entries);
        Ok(p)
    }

    pub fn copy(&mut self, from: &str, to: &str) -> Result<&Preset> {
        check_name(to)?;
        if self.presets.iter().any(|p| p.name == to) {
            bail!("preset \"{to}\" already exists");
        }
        let mut p = self.get(from)?.clone();
        p.name = to.into();
        self.presets.push(p);
        Ok(self.presets.last().unwrap())
    }

    pub fn delete(&mut self, name: &str) -> Result<()> {
        self.get(name)?;
        self.presets.retain(|p| p.name != name);
        Ok(())
    }
}

fn strings(options: &[OsString]) -> Result<Vec<String>> {
    options
        .iter()
        .map(|o| {
            o.to_str()
                .map(str::to_string)
                .with_context(|| format!("option {o:?} isn't valid UTF-8"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    fn opts(kind: Kind, v: &[&str]) -> Vec<String> {
        normalize(kind, v)
            .unwrap()
            .into_iter()
            .flat_map(|e| e.tokens)
            .collect()
    }

    #[test]
    fn normalizes_to_long_options() {
        assert_eq!(
            opts(
                Kind::Send,
                &["-g", "239.192.2.1:10000", "-pC", "-v", "-6", "-e", "g711u"]
            ),
            [
                "--group=239.192.2.1:10000",
                "--voice",
                "--chime=default",
                "--volume=-6",
                "--encoding=g711u"
            ]
        );
        assert!(normalize(Kind::Send, &["--group", "nonsense"]).is_err());
        assert!(normalize(Kind::Recv, &["--voice"]).is_err());
    }

    #[test]
    fn later_options_replace_conflicting_ones() {
        let cmd = Kind::Send.command();
        let base = normalize(Kind::Send, &["--tone", "440", "-p", "-g", "239.1.1.1:5000"]).unwrap();
        let over = normalize(Kind::Send, &["--file", "a.wav", "-m", "music"]).unwrap();
        let toks: Vec<String> = merge(&cmd, base, over)
            .into_iter()
            .flat_map(|e| e.tokens)
            .collect();
        assert_eq!(
            toks,
            ["--group=239.1.1.1:5000", "--file=a.wav", "--mode=music"]
        );
    }

    #[test]
    fn scans_unvalidated_command_lines() {
        let cmd = Kind::Send.command();
        let (ids, presets) = scan(
            &cmd,
            &os(&[
                "-P",
                "zone",
                "--loop",
                "-v",
                "-6",
                "-C",
                "--preset=voice",
                "-gx",
                "--wav",
                "f",
            ]),
        );
        assert_eq!(ids, ["looped", "volume", "chime", "group", "wav"]);
        assert_eq!(presets, ["zone", "voice"]);
    }

    #[test]
    fn file_round_trip() {
        let text = "# hi\n[page]\naudio send\n-g 239.192.2.1:10000\n--voice\n\n[lobby]\naudio recv\n--stream=Lobby music\n";
        let presets = parse_file(text).unwrap();
        assert_eq!(presets.len(), 2);
        assert_eq!(presets[0].options, ["-g 239.192.2.1:10000", "--voice"]);
        assert_eq!(presets[0].tokens(), ["-g", "239.192.2.1:10000", "--voice"]);
        assert_eq!(presets[1].kind, Kind::Recv);
        assert_eq!(
            presets[1].command_line(),
            "mcast audio recv '--stream=Lobby music'"
        );
        let again = parse_file(&format_file(&presets)).unwrap();
        assert_eq!(again[0].options, presets[0].options);
        assert!(parse_file("--voice\n").is_err());
        assert!(parse_file("[a]\naudio dance\n").is_err());
    }

    #[test]
    fn expands_presets_with_command_line_winning() {
        let dir = std::env::temp_dir().join(format!("mcast-presets-{}", std::process::id()));
        let path = dir.join("presets.conf");
        std::env::set_var("MCAST_PRESETS", &path);
        let mut s = Store::load().unwrap();
        s.create(
            "page",
            Kind::Send,
            &os(&["-g", "239.192.2.1:10000", "-e", "g711u", "--tone", "440"]),
        )
        .unwrap();
        s.set("voice", &os(&["-p", "-C", "-L", "800"])).unwrap();
        s.create("lobby", Kind::Recv, &os(&["-s", "lobby"]))
            .unwrap();
        s.save().unwrap();

        let run = |args: &[&str]| -> Vec<String> {
            let mut v = vec!["mcast"];
            v.extend(args);
            expand(os(&v))
                .unwrap()
                .into_iter()
                .map(|s| s.into_string().unwrap())
                .collect()
        };
        assert_eq!(
            run(&["@page", "--file", "x.wav", "-P", "voice", "-e", "g722"]),
            [
                "mcast",
                "audio",
                "send",
                "--group=239.192.2.1:10000",
                "--voice",
                "--chime=default",
                "--lead-in=800",
                "--file",
                "x.wav",
                "-P",
                "voice",
                "-e",
                "g722"
            ]
        );
        assert_eq!(
            run(&["preset", "run", "lobby", "-v", "3"]),
            ["mcast", "audio", "recv", "--stream=lobby", "-v", "3"]
        );
        assert_eq!(
            run(&["audio", "send", "--tone", "1000"]),
            ["mcast", "audio", "send", "--tone", "1000"]
        );
        assert!(expand(os(&["mcast", "audio", "recv", "-P", "page"])).is_err());
        assert!(expand(os(&["mcast", "@missing"])).is_err());

        let mut s = Store::load().unwrap();
        assert_eq!(
            s.unset("page", &["tone".into(), "-e".into()])
                .unwrap()
                .options,
            ["--group=239.192.2.1:10000"]
        );
        std::env::remove_var("MCAST_PRESETS");
        let _ = std::fs::remove_dir_all(dir);
    }
}
