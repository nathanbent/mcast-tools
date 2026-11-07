//! Files (and pictures) over multicast.
//!
//! Files are encoded with RaptorQ (RFC 6330), a fountain code: the sender
//! sends the file's pieces and then as many extra "repair" pieces as it
//! likes, and a receiver can rebuild the file from *any* set of pieces
//! slightly larger than the file, whichever ones it happened to catch. So
//! there are no retransmissions and no back channel, receivers with
//! different losses all finish, and a sender looping repair pieces lets
//! late joiners finish too.

pub mod preview;
pub mod receiver;
pub mod sender;
pub mod session;
pub mod wire;

pub use session::FileInfo;
