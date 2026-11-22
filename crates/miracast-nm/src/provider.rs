use crate::P2pError;
use std::future::Future;
use std::net::Ipv4Addr;
use std::pin::Pin;

/// Boxed future so [`P2pProvider`] stays object-safe (`dyn P2pProvider`).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A discovered P2P peer (a Miracast source or another sink).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    /// Opaque backend id (NM: the D-Bus object path). Pass it back to [`P2pProvider::connect`].
    pub id: String,
    pub name: String,
    /// P2P device address, `aa:bb:cc:dd:ee:ff`.
    pub hw_address: String,
    /// Raw WFD subelements the peer advertises (see [`crate::wfd`]); empty if none.
    pub wfd_ies: Vec<u8>,
    /// Signal strength 0-100.
    pub strength: u8,
}

/// WPS provisioning for group formation. PBC is the normal Miracast case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WpsMethod {
    #[default]
    Pbc,
    /// Let the backend pick.
    Auto,
}

/// Who owns the P2P group. The sink must work in both roles (miracast.md section 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupRole {
    /// We are the group owner (we serve DHCP).
    Owner,
    /// The source is the group owner (we got our address by DHCP from it).
    Client,
}

/// A formed P2P group and the addresses the RTSP/RTP code needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupInfo {
    /// Network interface of the group, e.g. `p2p-wlp3s0-0`.
    pub interface: String,
    pub role: GroupRole,
    pub local_ip: Ipv4Addr,
    /// The other side's address if already known (the GO address when we are the client).
    pub peer_ip: Option<Ipv4Addr>,
}

/// Something that happens without us asking (R1 in miracast.md: an incoming negotiation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum P2pEvent {
    PeerAdded(PeerInfo),
    /// Peer id.
    PeerRemoved(String),
    GroupFormed(GroupInfo),
    GroupEnded,
}

/// Platform-agnostic P2P access. All methods are cancel-safe.
pub trait P2pProvider: Send + Sync {
    /// Publish these WFD subelements so sources list us (see [`crate::wfd::sink_device_info`]).
    fn set_wfd_ies<'a>(&'a self, ies: &'a [u8]) -> BoxFuture<'a, Result<(), P2pError>>;
    /// Start P2P discovery.
    fn start_find(&self) -> BoxFuture<'_, Result<(), P2pError>>;
    fn stop_find(&self) -> BoxFuture<'_, Result<(), P2pError>>;
    /// Peers currently known.
    fn peers(&self) -> BoxFuture<'_, Result<Vec<PeerInfo>, P2pError>>;
    /// Actively connect to a peer and wait for the group.
    fn connect<'a>(
        &'a self,
        peer_id: &'a str,
        wps: WpsMethod,
    ) -> BoxFuture<'a, Result<GroupInfo, P2pError>>;
    /// Leave or dissolve the current group.
    fn disconnect(&self) -> BoxFuture<'_, Result<(), P2pError>>;
    /// Next event; `None` when the provider is closed.
    fn next_event(&self) -> BoxFuture<'_, Option<P2pEvent>>;
}
