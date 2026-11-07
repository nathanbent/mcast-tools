//! Uncompressed PCM audio over RTP multicast.
//!
//! Streams are RTP (RFC 3550) carrying big-endian linear PCM (L16 per
//! RFC 3551, L24 per RFC 3190), the same payloads AES67 uses.

pub mod codec;
pub mod convert;
pub mod device;
pub mod dsp;
pub mod file;
pub mod format;
pub mod jitter;
pub mod packetizer;
pub mod process;
pub mod receiver;
pub mod sender;
pub mod session;

pub use codec::{Codec, StreamSpec};
pub use format::{AudioFormat, Encoding};
pub use session::StreamInfo;
