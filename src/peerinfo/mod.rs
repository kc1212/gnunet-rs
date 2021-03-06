//! Module for connecting to and querying the GNUnet peerinfo services.

pub use self::peerinfo::{get_peers, get_peers_vec, get_peer, get_self_id, PeerIdentity};

pub mod peerinfo;

