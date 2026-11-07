//! Real-time text over RTP multicast.
//!
//! Text is sent as T.140 (UTF-8) in RTP per RFC 4103, normally with RFC 2198
//! redundancy: every packet repeats the previous two packets' text, so a
//! receiver can rebuild what it missed after losing up to two packets in a
//! row. Text is gathered for up to 300 ms per packet, and nothing is sent
//! while nobody is typing (RTCP keeps the heartbeat going).

pub mod lines;
pub mod receiver;
pub mod sender;
pub mod session;
pub mod t140;

pub use session::TextInfo;
