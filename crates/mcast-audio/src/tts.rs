//! Text to speech, by an outside program: Piper (natural, neural voices)
//! when it and a voice are installed, else eSpeak NG, or any command that
//! reads text on stdin and writes a WAV to stdout.
//!
//! What's spoken is kept in a cache directory, so the same words in the
//! same voice are only synthesized once.

use anyhow::{bail, Context, Result};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Longest text spoken at once, in characters.
pub const MAX_TEXT: usize = 2000;
/// How long synthesis may take.
const TIMEOUT: Duration = Duration::from_secs(60);
/// Files kept in the cache; the oldest go first past this.
const CACHE_FILES: usize = 500;

/// Where text to speech comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum Engine {
    /// Piper, with its voices (`NAME.onnx`, each beside its `NAME.onnx.json`).
    Piper { program: PathBuf, voices: PathBuf },
    /// eSpeak NG.
    Espeak { program: PathBuf },
    /// `sh -c COMMAND`: text on stdin, a WAV on stdout, `$MCAST_TTS_VOICE`
    /// set when a voice is asked for.
    Command(String),
}

impl Engine {
    /// A short name: `piper`, `espeak-ng` or `command`.
    pub fn name(&self) -> &'static str {
        match self {
            Engine::Piper { .. } => "piper",
            Engine::Espeak { .. } => "espeak-ng",
            Engine::Command(_) => "command",
        }
    }
}

/// How to find an engine.
#[derive(Debug, Clone, Default)]
pub struct TtsConfig {
    /// A command to use instead of Piper or eSpeak NG.
    pub command: Option<String>,
    /// Piper's voices; unset looks in [`DEFAULT_VOICES`].
    pub voices: Option<PathBuf>,
    /// The voice used when none is asked for.
    pub voice: Option<String>,
    /// Where spoken text is kept; unset is a directory under the system's
    /// temporary directory.
    pub cache: Option<PathBuf>,
}

/// Where Piper voices are looked for by default.
pub const DEFAULT_VOICES: &str = "/usr/local/share/mcast/voices";

/// A text-to-speech engine and its cache.
#[derive(Debug, Clone)]
pub struct Tts {
    engine: Engine,
    default_voice: Option<String>,
    cache: PathBuf,
}

/// What [`Tts::describe`] reports.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TtsInfo {
    pub engine: &'static str,
    /// Voices that can be asked for: Piper's installed voices, eSpeak NG's
    /// languages, nothing for a command.
    pub voices: Vec<String>,
    pub default_voice: Option<String>,
}

impl Tts {
    /// Find an engine: the command if one is given, else Piper if it's on
    /// the `PATH` with at least one voice, else eSpeak NG. `None` when
    /// there's none.
    pub fn detect(cfg: &TtsConfig) -> Option<Self> {
        let engine = match &cfg.command {
            Some(c) if !c.trim().is_empty() => Engine::Command(c.clone()),
            _ => {
                let voices = cfg
                    .voices
                    .clone()
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_VOICES));
                match find_program("piper") {
                    Some(program) if !piper_voices(&voices).is_empty() => {
                        Engine::Piper { program, voices }
                    }
                    _ => Engine::Espeak {
                        program: find_program("espeak-ng")?,
                    },
                }
            }
        };
        Some(Self::new(engine, cfg))
    }

    pub fn new(engine: Engine, cfg: &TtsConfig) -> Self {
        let cache = cfg
            .cache
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join(format!("mcast-tts-{}", user_tag())));
        Self {
            engine,
            default_voice: cfg.voice.clone().filter(|v| !v.trim().is_empty()),
            cache,
        }
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The engine and the voices it offers.
    pub fn describe(&self) -> TtsInfo {
        let voices = match &self.engine {
            Engine::Piper { voices, .. } => piper_voices(voices),
            Engine::Espeak { program } => espeak_voices(program),
            Engine::Command(_) => Vec::new(),
        };
        let default_voice = self.default_voice.clone().or_else(|| match &self.engine {
            Engine::Piper { .. } => voices.first().cloned(),
            _ => None,
        });
        TtsInfo {
            engine: self.engine.name(),
            voices,
            default_voice,
        }
    }

    /// Speak `text`, returning a WAV of it (from the cache when it's been
    /// spoken before).
    pub fn speak(&self, text: &str, voice: Option<&str>) -> Result<PathBuf> {
        let text = text.trim();
        if text.is_empty() {
            bail!("there's no text to speak");
        }
        if text.chars().count() > MAX_TEXT {
            bail!("text to speak is at most {MAX_TEXT} characters");
        }
        let voice = voice
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .or_else(|| self.default_voice.clone());
        if let Some(v) = &voice {
            if v.len() > 100
                || v.starts_with('.')
                || !v
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.+".contains(c))
            {
                bail!("\"{v}\" isn't a voice name");
            }
        }
        make_private_dir(&self.cache)?;
        let file = self
            .cache
            .join(format!("{}.wav", self.key(text, voice.as_deref())));
        if file.is_file() {
            // Touch it, so the cache keeps what's used.
            let _ = std::fs::File::options()
                .append(true)
                .open(&file)
                .and_then(|f| f.set_modified(std::time::SystemTime::now()));
            return Ok(file);
        }
        let partial = self
            .cache
            .join(format!(".{}-{}.part", std::process::id(), unique()));
        let result = self.synthesize(text, voice.as_deref(), &partial);
        let result = result.and_then(|()| {
            check_wav(&partial)?;
            std::fs::rename(&partial, &file).context("saving the speech")
        });
        if result.is_err() {
            let _ = std::fs::remove_file(&partial);
        }
        result?;
        prune(&self.cache);
        Ok(file)
    }

    fn key(&self, text: &str, voice: Option<&str>) -> String {
        let engine = match &self.engine {
            Engine::Piper { voices, .. } => format!("piper:{}", voices.display()),
            Engine::Espeak { .. } => "espeak".into(),
            Engine::Command(c) => format!("command:{c}"),
        };
        let mut out = String::new();
        for salt in ["a", "b"] {
            let mut h = DefaultHasher::new();
            (salt, &engine, voice, text).hash(&mut h);
            out.push_str(&format!("{:016x}", h.finish()));
        }
        out
    }

    fn synthesize(&self, text: &str, voice: Option<&str>, out: &Path) -> Result<()> {
        let (mut cmd, stdout_wav) = match &self.engine {
            Engine::Piper { program, voices } => {
                let names = piper_voices(voices);
                let name = match voice {
                    Some(v) => names
                        .iter()
                        .find(|n| n.eq_ignore_ascii_case(v))
                        .with_context(|| format!("there's no Piper voice \"{v}\""))?,
                    None => names.first().context("there are no Piper voices")?,
                };
                let mut c = Command::new(program);
                c.arg("-m")
                    .arg(voices.join(format!("{name}.onnx")))
                    .arg("-f")
                    .arg(out);
                (c, false)
            }
            Engine::Espeak { program } => {
                let mut c = Command::new(program);
                if let Some(v) = voice {
                    c.arg("-v").arg(v);
                }
                c.arg("-w").arg(out).arg("--stdin");
                (c, false)
            }
            Engine::Command(command) => {
                let mut c = Command::new("sh");
                c.arg("-c").arg(command);
                if let Some(v) = voice {
                    c.env("MCAST_TTS_VOICE", v);
                }
                (c, true)
            }
        };
        cmd.stdin(Stdio::piped()).stderr(Stdio::piped());
        if stdout_wav {
            let f = std::fs::File::create(out).context("creating the speech file")?;
            cmd.stdout(f);
        } else {
            cmd.stdout(Stdio::null());
        }
        let name = self.engine.name();
        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting {name} for text to speech"))?;
        {
            let mut stdin = child.stdin.take().expect("piped");
            // An engine that quits early is reported by its exit status.
            let _ = stdin.write_all(text.as_bytes());
            let _ = stdin.write_all(b"\n");
        }
        let stderr = child.stderr.take();
        let err_reader = std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut e) = stderr {
                use std::io::Read;
                let _ = e.by_ref().take(8192).read_to_string(&mut s);
            }
            s
        });
        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break s;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!("{name} took more than {}s to speak", TIMEOUT.as_secs());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let err = err_reader.join().unwrap_or_default();
        if !status.success() {
            let last = err
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("");
            bail!(
                "{name} failed ({status}){}",
                if last.is_empty() {
                    String::new()
                } else {
                    format!(": {}", last.trim())
                }
            );
        }
        Ok(())
    }
}

/// Check what an engine wrote is a WAV we can read, with some audio in it.
fn check_wav(path: &Path) -> Result<()> {
    let f = crate::file::AudioFile::open(path).context("the speech isn't a readable WAV")?;
    if f.frames() == Some(0) {
        bail!("the speech came out empty");
    }
    Ok(())
}

/// Piper voices in `dir`: each `NAME.onnx` with its `NAME.onnx.json`, sorted.
pub fn piper_voices(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.strip_suffix(".onnx")?.to_string();
            (!name.starts_with('.') && dir.join(format!("{name}.onnx.json")).is_file())
                .then_some(name)
        })
        .collect();
    out.sort();
    out
}

/// eSpeak NG's languages (`espeak-ng --voices`), e.g. `en-us`.
fn espeak_voices(program: &Path) -> Vec<String> {
    let Ok(out) = Command::new(program)
        .arg("--voices")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    let mut v: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1)
        .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
        .collect();
    v.sort();
    v.dedup();
    v
}

/// A program on the `PATH`.
pub fn find_program(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|p| is_executable(p))
    })
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

fn make_private_dir(dir: &Path) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
        .with_context(|| format!("creating {}", dir.display()))
}

/// Keep the cache to [`CACHE_FILES`], dropping the least recently used.
fn prune(dir: &Path) {
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "wav"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.len() <= CACHE_FILES {
        return;
    }
    files.sort();
    for (_, p) in &files[..files.len() - CACHE_FILES] {
        let _ = std::fs::remove_file(p);
    }
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

/// Tells users' caches apart in a shared temporary directory.
fn user_tag() -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(m) = std::fs::metadata("/proc/self") {
            return m.uid().to_string();
        }
    }
    std::env::var("USER").unwrap_or_else(|_| "user".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command that writes a short WAV, whatever it's told.
    fn fake(dir: &Path) -> Tts {
        let wav = dir.join("beep.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&wav, spec).unwrap();
        for i in 0..1600 {
            w.write_sample(((i as f32 * 0.2).sin() * 8000.0) as i16)
                .unwrap();
        }
        w.finalize().unwrap();
        let log = dir.join("calls");
        let cfg = TtsConfig {
            command: Some(format!(
                "cat >> '{}'; echo \"$MCAST_TTS_VOICE\" >> '{}'; cat '{}'",
                log.display(),
                log.display(),
                wav.display()
            )),
            cache: Some(dir.join("cache")),
            ..Default::default()
        };
        Tts::detect(&cfg).unwrap()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mcast-tts-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn speaks_through_a_command_and_caches() {
        let dir = temp_dir("cmd");
        let tts = fake(&dir);
        assert_eq!(tts.engine().name(), "command");
        let a = tts.speak("Fire drill at three", None).unwrap();
        let again = tts.speak("  Fire drill at three ", None).unwrap();
        assert_eq!(a, again);
        let b = tts.speak("Fire drill at three", Some("en-gb")).unwrap();
        assert_ne!(a, b);
        assert!(crate::file::AudioFile::open(&a).is_ok());
        // Synthesized twice: once per voice.
        let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
        assert_eq!(calls, "Fire drill at three\n\nFire drill at three\nen-gb\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("cache"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_bad_input_and_failing_engines() {
        let dir = temp_dir("bad");
        let tts = fake(&dir);
        assert!(tts.speak("   ", None).is_err());
        assert!(tts.speak(&"x".repeat(MAX_TEXT + 1), None).is_err());
        assert!(tts.speak("hi", Some("../etc")).is_err());
        let broken = Tts::new(
            Engine::Command("echo nope >&2; exit 3".into()),
            &TtsConfig {
                cache: Some(dir.join("cache2")),
                ..Default::default()
            },
        );
        let e = broken.speak("hi", None).unwrap_err().to_string();
        assert!(e.contains("nope"), "{e}");
        let junk = Tts::new(
            Engine::Command("echo not a wav".into()),
            &TtsConfig {
                cache: Some(dir.join("cache3")),
                ..Default::default()
            },
        );
        assert!(junk.speak("hi", None).is_err());
        // Nothing half-written is left behind.
        assert_eq!(std::fs::read_dir(dir.join("cache3")).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lists_piper_voices_with_their_configs() {
        let dir = temp_dir("voices");
        for f in [
            "en_US-amy-medium.onnx",
            "en_US-amy-medium.onnx.json",
            "lone.onnx",
            "de_DE-thorsten-low.onnx",
            "de_DE-thorsten-low.onnx.json",
        ] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        assert_eq!(
            piper_voices(&dir),
            ["de_DE-thorsten-low", "en_US-amy-medium"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "needs espeak-ng installed"]
    fn speaks_with_espeak() {
        let dir = temp_dir("espeak");
        let tts = Tts::new(
            Engine::Espeak {
                program: find_program("espeak-ng").unwrap(),
            },
            &TtsConfig {
                cache: Some(dir.join("c")),
                ..Default::default()
            },
        );
        let f = tts.speak("Hello there", Some("en-us")).unwrap();
        assert!(crate::file::AudioFile::open(&f).unwrap().frames().unwrap() > 1000);
        assert!(tts.describe().voices.iter().any(|v| v == "en-us"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
