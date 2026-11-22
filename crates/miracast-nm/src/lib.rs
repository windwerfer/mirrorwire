#![forbid(unsafe_code)]
//! Rootless Wi-Fi Direct (P2P) for a Miracast sink (spike, `docs/agents/miracast.md`).
//!
//! The core is platform-agnostic: [`P2pProvider`] is the only thing the sink logic sees.
//! [`MockP2pProvider`] drives tests; [`NmP2pProvider`] maps the trait onto NetworkManager
//! calls through the small [`NmApi`] trait, which the Linux-only `zbus_backend` implements
//! on the system bus (no root, no stopping NetworkManager).

mod error;
pub mod freq;
mod mock;
mod nm;
mod provider;
mod sink;
pub mod wfd;
#[cfg(target_os = "linux")]
pub mod zbus_backend;

pub use error::P2pError;
pub use mock::{MockCall, MockP2pProvider};
pub use nm::{
    group_from_ip4, ActiveState, GroupIp4, MockNm, NmApi, NmP2pProvider, NmPeer, P2pConnectionSpec,
    DEFAULT_GROUP_TIMEOUT,
};
pub use provider::{BoxFuture, GroupInfo, GroupRole, P2pEvent, P2pProvider, PeerInfo, WpsMethod};
pub use sink::{wfd_source_port, Sink, SinkConfig};
