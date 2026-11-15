//! Real-time text over RTP multicast.
//!
//! Text is sent as T.140 (UTF-8) in RTP per RFC 4103, normally with RFC 2198
//! redundancy: every packet repeats the previous two packets' text, so a
//! receiver can rebuild what it missed after losing up to two packets in a
//! row. Text is gathered for up to 300 ms per packet, and nothing is sent
//! while nobody is typing (RTCP keeps the heartbeat going).

//!
//! The layers, bottom up: [`t140`] is the payload format, [`sender`] and
//! [`receiver`] put it on and take it off the network, [`lines`] turns one
//! sender's characters into lines, and [`transcript`] turns everything a
//! receiver hears into who said what. Text we send goes through
//! [`mcast_plain::shown`] first; text we receive is made safe to show as
//! each line completes.

pub mod lines;
pub mod receiver;
pub mod sender;
pub mod session;
pub mod t140;
pub mod transcript;

pub use session::TextInfo;
pub use transcript::{Entry, Transcript};

/// Text's own default group, away from the audio and paging groups.
pub const DEFAULT_GROUP: &str = "239.69.1.1:5004";
