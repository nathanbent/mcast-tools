//! Reading audio files: WAV, MP3, FLAC, Ogg Vorbis, AAC/M4A, ALAC and more,
//! decoded with Symphonia to interleaved f32.

use anyhow::{anyhow, bail, Context, Result};
use std::fs::File;
use std::path::{Path, PathBuf};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// What an audio file holds, without playing it.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioInfo {
    /// The codec's short name: `pcm_s16le`, `mp3`, `flac`, `vorbis`, `aac`...
    pub codec: String,
    pub rate: u32,
    pub channels: u16,
    pub duration_s: f64,
}

/// Open and check an audio file: its codec, rate, channels and length.
/// When the container doesn't say how long it is, the whole file is
/// decoded to find out. An error if it can't be played.
pub fn probe(path: &Path) -> Result<AudioInfo> {
    let mut f = AudioFile::open(path)?;
    let frames = match f.frames {
        Some(n) if n > 0 => n,
        _ => {
            let mut buf = vec![0.0f32; 16_384];
            let mut samples = 0u64;
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                samples += n as u64;
            }
            samples / f.channels.max(1) as u64
        }
    };
    if frames == 0 {
        bail!("{}: no audio in it", path.display());
    }
    Ok(AudioInfo {
        codec: f.codec.clone(),
        rate: f.sample_rate,
        channels: f.channels,
        duration_s: frames as f64 / f.sample_rate.max(1) as f64,
    })
}

pub struct AudioFile {
    path: PathBuf,
    /// Frames in the file, if the container says.
    frames: Option<u64>,
    codec: String,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    sample_rate: u32,
    channels: u16,
    /// Decoded samples not yet handed out.
    pending: Vec<f32>,
    pending_pos: usize,
    ended: bool,
}

impl AudioFile {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(|e| {
                anyhow!(
                    "{}: unsupported or unreadable audio file ({e})",
                    path.display()
                )
            })?;
        let format = probed.format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or_else(|| anyhow!("{}: no audio track", path.display()))?;
        let track_id = track.id;
        let params = track.codec_params.clone();
        let decoder = symphonia::default::get_codecs()
            .make(&params, &DecoderOptions::default())
            .map_err(|e| anyhow!("{}: can't decode this audio ({e})", path.display()))?;
        let codec = symphonia::default::get_codecs()
            .get_codec(params.codec)
            .map_or_else(|| "unknown".to_string(), |d| d.short_name.to_string());
        let mut f = Self {
            path: path.to_path_buf(),
            frames: params.n_frames,
            codec,
            format,
            decoder,
            track_id,
            sample_rate: params.sample_rate.unwrap_or(0),
            channels: params.channels.map_or(0, |c| c.count() as u16),
            pending: Vec::new(),
            pending_pos: 0,
            ended: false,
        };
        // Some formats only reveal rate/channels once decoding starts.
        if f.sample_rate == 0 || f.channels == 0 {
            f.decode_next()?;
            if f.sample_rate == 0 || f.channels == 0 {
                bail!(
                    "{}: couldn't determine sample rate and channels",
                    path.display()
                );
            }
        }
        Ok(f)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Frames in the file, if the container says.
    pub fn frames(&self) -> Option<u64> {
        self.frames
    }

    /// Decode the next packet into `pending`; false at the end of the file.
    fn decode_next(&mut self) -> Result<bool> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(false)
                }
                Err(SymError::ResetRequired) => return Ok(false),
                Err(e) => return Err(anyhow!("{}: {e}", self.path.display())),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                // A corrupt frame: skip it, as players do.
                Err(SymError::DecodeError(_)) => continue,
                Err(e) => return Err(anyhow!("{}: {e}", self.path.display())),
            };
            let spec = *decoded.spec();
            if self.sample_rate == 0 {
                self.sample_rate = spec.rate;
            }
            if self.channels == 0 {
                self.channels = spec.channels.count() as u16;
            }
            if decoded.frames() == 0 {
                continue;
            }
            let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
            buf.copy_interleaved_ref(decoded);
            self.pending.drain(..self.pending_pos);
            self.pending_pos = 0;
            self.pending.extend_from_slice(buf.samples());
            return Ok(true);
        }
    }

    /// Fill `out` with interleaved samples; returns how many were written
    /// (fewer than `out.len()` only at the end of the file).
    pub fn read(&mut self, out: &mut [f32]) -> Result<usize> {
        let mut written = 0;
        while written < out.len() {
            if self.pending_pos == self.pending.len() && (self.ended || !self.decode_next()?) {
                self.ended = true;
                break;
            }
            let n = (self.pending.len() - self.pending_pos).min(out.len() - written);
            out[written..written + n]
                .copy_from_slice(&self.pending[self.pending_pos..self.pending_pos + n]);
            self.pending_pos += n;
            written += n;
        }
        Ok(written)
    }

    /// Skip `frames` frames; returns false if the file ended first.
    pub fn skip(&mut self, frames: u64) -> Result<bool> {
        let mut left = frames * self.channels as u64;
        let mut scratch = vec![0.0f32; 16_384];
        while left > 0 {
            let want = left.min(scratch.len() as u64) as usize;
            let got = self.read(&mut scratch[..want])?;
            if got == 0 {
                return Ok(false);
            }
            left -= got as u64;
        }
        Ok(true)
    }

    /// Start again from the beginning.
    pub fn rewind(&mut self) -> Result<()> {
        *self = Self::open(&self.path.clone())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &Path, rate: u32, channels: u16, frames: usize) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..frames * channels as usize {
            w.write_sample(((i % 100) as i16 - 50) * 100).unwrap();
        }
        w.finalize().unwrap();
    }

    #[test]
    fn probes_length_and_refuses_what_isnt_audio() {
        let dir = std::env::temp_dir().join(format!("mcast-probe-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("t.wav");
        write_wav(&wav, 8000, 1, 12_000);
        let i = probe(&wav).unwrap();
        assert_eq!(
            (i.rate, i.channels, i.codec.as_str()),
            (8000, 1, "pcm_s16le")
        );
        assert!((i.duration_s - 1.5).abs() < 1e-9, "{i:?}");
        let empty = dir.join("empty.wav");
        write_wav(&empty, 8000, 1, 0);
        assert!(probe(&empty).is_err());
        let text = dir.join("notes.wav");
        std::fs::write(&text, "not audio at all").unwrap();
        assert!(probe(&text).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reads_wav_exactly_and_rewinds() {
        let dir = std::env::temp_dir().join(format!("mcast-file-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        write_wav(&path, 22_050, 2, 1000);
        let mut f = AudioFile::open(&path).unwrap();
        assert_eq!((f.sample_rate(), f.channels()), (22_050, 2));
        let mut out = vec![0.0; 5000];
        assert_eq!(f.read(&mut out).unwrap(), 2000);
        assert!((out[0] - (-5000.0 / 32768.0)).abs() < 1e-6);
        assert!((out[1] - (-4900.0 / 32768.0)).abs() < 1e-6);
        f.rewind().unwrap();
        assert!(f.skip(999).unwrap());
        assert!(!f.skip(2).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
