//! Shared building blocks for mcast-tools: multicast sockets, RTP, SAP and SDP.

pub mod net;
pub mod rtcp;
pub mod rtp;
pub mod sap;
pub mod sdp;

pub use net::{
    enable_packet_info, open_receiver, open_receiver_groups, open_sender, recv_with_info,
    Interface, MulticastOptions, Received,
};

/// Non-cryptographic random value (SSRCs, session IDs, initial sequence numbers).
pub fn random_u32() -> u32 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut h = RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.finish() as u32
}
