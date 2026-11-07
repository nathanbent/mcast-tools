//! Shared building blocks for mcast-tools: multicast sockets and RTP.

pub mod net;
pub mod rtp;

pub use net::{open_receiver, open_sender, Interface, MulticastOptions};
