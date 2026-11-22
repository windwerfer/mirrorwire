//! NetworkManager mapping of [`P2pProvider`]. NM's object model (devices, peers, connection
//! profiles) is behind [`NmApi`] so the logic here is tested against a recording mock; the
//! real D-Bus calls live in `zbus_backend`.

use crate::{
    BoxFuture, GroupInfo, GroupRole, P2pError, P2pEvent, P2pProvider, PeerInfo, WpsMethod,
};
use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::sync::Mutex;
use std::time::Duration;

/// A peer as NM reports it (`org.freedesktop.NetworkManager.WifiP2PPeer`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NmPeer {
    /// D-Bus object path.
    pub path: String,
    pub name: String,
    pub hw_address: String,
    pub wfd_ies: Vec<u8>,
    pub strength: u8,
}

/// The volatile `wifi-p2p` connection profile we ask NM to activate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P2pConnectionSpec {
    /// `connection.id`.
    pub id: String,
    /// `wifi-p2p.peer`: the peer's P2P device address.
    pub peer: String,
    pub wps: WpsMethod,
    /// `wifi-p2p.wfd-ies`.
    pub wfd_ies: Vec<u8>,
}

impl P2pConnectionSpec {
    /// NM `NMSettingWifiP2PWpsMethod` value for `wifi-p2p.wps-method`.
    pub fn wps_method_value(&self) -> u32 {
        match self.wps {
            WpsMethod::Auto => 0x0, // default: NM decides
            WpsMethod::Pbc => 0x4,
        }
    }
}

/// `NM_ACTIVE_CONNECTION_STATE_*` of an active connection, plus `Gone` when NM removed the
/// object (a volatile profile that failed disappears at once; seen on hardware).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveState {
    Unknown,
    Activating,
    Activated,
    Deactivating,
    Deactivated,
    Gone,
}

impl ActiveState {
    /// Maps NM's `State` property value (0-4); anything else is `Unknown`.
    pub fn from_nm(v: u32) -> Self {
        match v {
            1 => Self::Activating,
            2 => Self::Activated,
            3 => Self::Deactivating,
            4 => Self::Deactivated,
            _ => Self::Unknown,
        }
    }
}

/// IPv4 configuration of the group interface once NM has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupIp4 {
    /// The group's IP interface, e.g. `p2p-wlp58s0-3`.
    pub interface: String,
    /// Our addresses with prefix length.
    pub addresses: Vec<(Ipv4Addr, u8)>,
    /// Set when we are a P2P client: the group owner (on hardware the source, `192.168.49.1`).
    pub gateway: Option<Ipv4Addr>,
}

/// Turns the group's IPv4 config into [`GroupInfo`]. A gateway that is not our own address
/// means the peer is group owner and the source; no gateway means we are the owner and the
/// source's address is learned later (from the RTSP side).
pub fn group_from_ip4(ip: &GroupIp4) -> Result<GroupInfo, P2pError> {
    let &(local_ip, _) = ip.addresses.first().ok_or_else(|| {
        P2pError::Backend(format!(
            "the Wi-Fi Direct group on {} has no IPv4 address",
            ip.interface
        ))
    })?;
    let (role, peer_ip) = match ip.gateway {
        Some(gw) if gw != local_ip && !gw.is_unspecified() => (GroupRole::Client, Some(gw)),
        _ => (GroupRole::Owner, None),
    };
    Ok(GroupInfo {
        interface: ip.interface.clone(),
        role,
        local_ip,
        peer_ip,
    })
}

/// The NetworkManager operations the provider needs. Implemented by the zbus backend (Linux)
/// and by test mocks. `device` is the D-Bus path of a `WifiP2P` device, `active` the path of
/// an active connection.
///
/// WFD IEs are not set on the device (NM has no such property; hardware run 1): they travel
/// in the `wifi-p2p.wfd-ies` setting of the profile given to [`activate`](Self::activate),
/// and NM publishes them only while that profile is activating or active (risk R2, settled).
pub trait NmApi: Send + Sync {
    /// Paths of devices of type WIFI_P2P; empty if the hardware or supplicant lacks P2P.
    fn p2p_devices(&self) -> BoxFuture<'_, Result<Vec<String>, P2pError>>;
    /// `Device.WifiP2P.StartFind` with a timeout.
    fn start_find<'a>(
        &'a self,
        device: &'a str,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<(), P2pError>>;
    fn stop_find<'a>(&'a self, device: &'a str) -> BoxFuture<'a, Result<(), P2pError>>;
    fn peers<'a>(&'a self, device: &'a str) -> BoxFuture<'a, Result<Vec<NmPeer>, P2pError>>;
    /// `AddAndActivateConnection2` with `persist: volatile` (NM forgets the profile when it
    /// deactivates). Returns the active connection path.
    fn activate<'a>(
        &'a self,
        device: &'a str,
        spec: &'a P2pConnectionSpec,
    ) -> BoxFuture<'a, Result<String, P2pError>>;
    fn active_state<'a>(&'a self, active: &'a str) -> BoxFuture<'a, Result<ActiveState, P2pError>>;
    /// The group's IPv4 config; `None` while NM has none yet.
    fn group_ip4<'a>(
        &'a self,
        active: &'a str,
    ) -> BoxFuture<'a, Result<Option<GroupIp4>, P2pError>>;
    /// `DeactivateConnection`; an already gone connection is not an error.
    fn deactivate<'a>(&'a self, active: &'a str) -> BoxFuture<'a, Result<(), P2pError>>;
    /// Wait between polls (the mock returns at once).
    fn sleep(&self, d: Duration) -> BoxFuture<'_, ()>;
    /// Next change signal translated to an event; `None` when closed.
    fn next_event(&self) -> BoxFuture<'_, Option<P2pEvent>>;
}

/// How long one find runs before the backend must restart it.
const FIND_TIMEOUT: Duration = Duration::from_secs(120);
/// Poll interval while waiting for the group.
const POLL: Duration = Duration::from_millis(250);
/// Default wait for the group: the user has to tap us in the source's Cast list (R1).
pub const DEFAULT_GROUP_TIMEOUT: Duration = Duration::from_secs(120);
/// Wait for the IPv4 config after the connection is activated.
const IP4_TIMEOUT: Duration = Duration::from_secs(10);

/// [`P2pProvider`] over NetworkManager. Uses the first P2P device.
pub struct NmP2pProvider<A: NmApi> {
    api: A,
    /// WFD IEs last set; put into the connection profile (the only way NM publishes them).
    wfd_ies: Mutex<Vec<u8>>,
    /// The active connection we started, for [`P2pProvider::disconnect`].
    active: Mutex<Option<String>>,
    group_timeout: Duration,
}

impl<A: NmApi> NmP2pProvider<A> {
    pub fn new(api: A) -> Self {
        Self {
            api,
            wfd_ies: Mutex::new(Vec::new()),
            active: Mutex::new(None),
            group_timeout: DEFAULT_GROUP_TIMEOUT,
        }
    }

    /// How long [`P2pProvider::connect`] waits for the user to accept on the source.
    pub fn with_group_timeout(mut self, t: Duration) -> Self {
        self.group_timeout = t;
        self
    }

    pub fn api(&self) -> &A {
        &self.api
    }

    async fn device(&self) -> Result<String, P2pError> {
        self.api
            .p2p_devices()
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                P2pError::NotAvailable(
                    "NetworkManager has no Wi-Fi P2P device (check wpa_supplicant P2P support)"
                        .into(),
                )
            })
    }

    fn ies(&self) -> Vec<u8> {
        self.wfd_ies
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_active(&self, a: Option<String>) {
        *self.active.lock().unwrap_or_else(|e| e.into_inner()) = a;
    }

    fn polls(d: Duration) -> u128 {
        (d.as_millis() / POLL.as_millis()).max(1)
    }

    /// Waits until `active` is activated and has an IPv4 address (R1 flow: NM stays
    /// "activating" until the user taps us on the source).
    async fn wait_for_group(&self, active: &str, peer: &NmPeer) -> Result<GroupInfo, P2pError> {
        let mut activated = false;
        for _ in 0..Self::polls(self.group_timeout) {
            match self.api.active_state(active).await? {
                ActiveState::Activated => {
                    activated = true;
                    break;
                }
                ActiveState::Unknown | ActiveState::Activating => {}
                ActiveState::Deactivating | ActiveState::Deactivated | ActiveState::Gone => {
                    return Err(P2pError::Backend(format!(
                        "the Wi-Fi Direct group with {} did not form (this happens every few \
                         attempts on real hardware, or the source declined); try again: on the \
                         source, open Cast and tap this device, not the connect prompt",
                        peer.name
                    )));
                }
            }
            self.api.sleep(POLL).await;
        }
        if !activated {
            return Err(P2pError::Timeout(format!(
                "no Wi-Fi Direct group with {} after {} s: on the source, open Cast and tap \
                 this device",
                peer.name,
                self.group_timeout.as_secs()
            )));
        }
        for _ in 0..Self::polls(IP4_TIMEOUT) {
            if let Some(ip) = self.api.group_ip4(active).await? {
                if !ip.addresses.is_empty() {
                    return group_from_ip4(&ip);
                }
            }
            self.api.sleep(POLL).await;
        }
        Err(P2pError::Timeout(format!(
            "the Wi-Fi Direct group with {} got no IPv4 address in {} s",
            peer.name,
            IP4_TIMEOUT.as_secs()
        )))
    }
}

impl From<NmPeer> for PeerInfo {
    fn from(p: NmPeer) -> Self {
        PeerInfo {
            id: p.path,
            name: p.name,
            hw_address: p.hw_address,
            wfd_ies: p.wfd_ies,
            strength: p.strength,
        }
    }
}

impl<A: NmApi> P2pProvider for NmP2pProvider<A> {
    /// Only remembers the IEs: NM publishes them through the profile that
    /// [`P2pProvider::connect`] activates.
    fn set_wfd_ies<'a>(&'a self, ies: &'a [u8]) -> BoxFuture<'a, Result<(), P2pError>> {
        *self.wfd_ies.lock().unwrap_or_else(|e| e.into_inner()) = ies.to_vec();
        Box::pin(async { Ok(()) })
    }

    fn start_find(&self) -> BoxFuture<'_, Result<(), P2pError>> {
        Box::pin(async move {
            let dev = self.device().await?;
            self.api.start_find(&dev, FIND_TIMEOUT).await
        })
    }

    fn stop_find(&self) -> BoxFuture<'_, Result<(), P2pError>> {
        Box::pin(async move {
            let dev = self.device().await?;
            self.api.stop_find(&dev).await
        })
    }

    fn peers(&self) -> BoxFuture<'_, Result<Vec<PeerInfo>, P2pError>> {
        Box::pin(async move {
            let dev = self.device().await?;
            Ok(self
                .api
                .peers(&dev)
                .await?
                .into_iter()
                .map(Into::into)
                .collect())
        })
    }

    /// Activates a volatile `wifi-p2p` profile for the peer carrying our WFD IEs, waits for
    /// the group and reads its addresses. On any failure after activation the connection is
    /// deactivated again (nothing is left behind).
    fn connect<'a>(
        &'a self,
        peer_id: &'a str,
        wps: WpsMethod,
    ) -> BoxFuture<'a, Result<GroupInfo, P2pError>> {
        Box::pin(async move {
            let dev = self.device().await?;
            let peer = self
                .api
                .peers(&dev)
                .await?
                .into_iter()
                .find(|p| p.path == peer_id)
                .ok_or_else(|| P2pError::PeerNotFound(peer_id.to_string()))?;
            let spec = P2pConnectionSpec {
                id: format!("mirrorwire-miracast-{}", peer.hw_address),
                peer: peer.hw_address.clone(),
                wps,
                wfd_ies: self.ies(),
            };
            let active = self.api.activate(&dev, &spec).await?;
            self.set_active(Some(active.clone()));
            match self.wait_for_group(&active, &peer).await {
                Ok(g) => Ok(g),
                Err(e) => {
                    // Best effort: the error the user needs is `e`, not the cleanup's.
                    let _ = self.api.deactivate(&active).await;
                    self.set_active(None);
                    Err(e)
                }
            }
        })
    }

    fn disconnect(&self) -> BoxFuture<'_, Result<(), P2pError>> {
        Box::pin(async move {
            let active = self.active.lock().unwrap_or_else(|e| e.into_inner()).take();
            match active {
                Some(a) => self.api.deactivate(&a).await,
                None => Ok(()),
            }
        })
    }

    fn next_event(&self) -> BoxFuture<'_, Option<P2pEvent>> {
        self.api.next_event()
    }
}

/// Recording [`NmApi`] used by this module's tests and available to other crates' tests.
/// `states` is consumed one per `active_state` call; the last one repeats (empty: Activated).
#[derive(Default)]
pub struct MockNm {
    pub devices: Vec<String>,
    pub peers: Vec<NmPeer>,
    pub states: Mutex<VecDeque<ActiveState>>,
    /// Returned by `group_ip4` after `ip4_delay` calls that return `None`.
    pub ip4: Option<GroupIp4>,
    pub ip4_delay: Mutex<u32>,
    calls: Mutex<Vec<String>>,
    events: Mutex<VecDeque<P2pEvent>>,
    specs: Mutex<Vec<P2pConnectionSpec>>,
}

impl MockNm {
    fn log(&self, s: String) {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).push(s);
    }
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn specs(&self) -> Vec<P2pConnectionSpec> {
        self.specs.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn push_event(&self, e: P2pEvent) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(e);
    }
}

const MOCK_ACTIVE: &str = "/org/freedesktop/NetworkManager/ActiveConnection/7";

impl NmApi for MockNm {
    fn p2p_devices(&self) -> BoxFuture<'_, Result<Vec<String>, P2pError>> {
        Box::pin(async { Ok(self.devices.clone()) })
    }
    fn start_find<'a>(
        &'a self,
        device: &'a str,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<(), P2pError>> {
        self.log(format!("StartFind {device} {}s", timeout.as_secs()));
        Box::pin(async { Ok(()) })
    }
    fn stop_find<'a>(&'a self, device: &'a str) -> BoxFuture<'a, Result<(), P2pError>> {
        self.log(format!("StopFind {device}"));
        Box::pin(async { Ok(()) })
    }
    fn peers<'a>(&'a self, _device: &'a str) -> BoxFuture<'a, Result<Vec<NmPeer>, P2pError>> {
        Box::pin(async { Ok(self.peers.clone()) })
    }
    fn activate<'a>(
        &'a self,
        device: &'a str,
        spec: &'a P2pConnectionSpec,
    ) -> BoxFuture<'a, Result<String, P2pError>> {
        self.log(format!("AddAndActivate2 {device} peer={}", spec.peer));
        self.specs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(spec.clone());
        Box::pin(async { Ok(MOCK_ACTIVE.to_string()) })
    }
    fn active_state<'a>(
        &'a self,
        _active: &'a str,
    ) -> BoxFuture<'a, Result<ActiveState, P2pError>> {
        let mut q = self.states.lock().unwrap_or_else(|e| e.into_inner());
        let s = if q.len() > 1 {
            q.pop_front()
        } else {
            q.front().copied()
        }
        .unwrap_or(ActiveState::Activated);
        Box::pin(async move { Ok(s) })
    }
    fn group_ip4<'a>(
        &'a self,
        _active: &'a str,
    ) -> BoxFuture<'a, Result<Option<GroupIp4>, P2pError>> {
        let mut d = self.ip4_delay.lock().unwrap_or_else(|e| e.into_inner());
        let r = if *d > 0 {
            *d -= 1;
            None
        } else {
            self.ip4.clone()
        };
        Box::pin(async { Ok(r) })
    }
    fn deactivate<'a>(&'a self, active: &'a str) -> BoxFuture<'a, Result<(), P2pError>> {
        self.log(format!("Deactivate {active}"));
        Box::pin(async { Ok(()) })
    }
    fn sleep(&self, _d: Duration) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn next_event(&self) -> BoxFuture<'_, Option<P2pEvent>> {
        let e = self
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front();
        Box::pin(async { e })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEV: &str = "/org/freedesktop/NetworkManager/Devices/4";

    /// The TB336FU as NM listed it in hardware run 1 (`peer1.txt`).
    fn peer() -> NmPeer {
        NmPeer {
            path: "/org/freedesktop/NetworkManager/WifiP2PPeer/3".into(),
            name: "Android_qqQ0".into(),
            hw_address: "7A:1B:85:73:E1:76".into(),
            wfd_ies: vec![0, 0, 6, 0, 0x10, 0x1c, 0x44, 0, 0x32],
            strength: 97,
        }
    }

    /// Group as seen in hardware run 2: we are client, the tablet is GO and gateway.
    fn client_ip4() -> GroupIp4 {
        GroupIp4 {
            interface: "p2p-wlp58s0-3".into(),
            addresses: vec![(Ipv4Addr::new(192, 168, 49, 74), 24)],
            gateway: Some(Ipv4Addr::new(192, 168, 49, 1)),
        }
    }

    fn provider_with(states: &[ActiveState], ip4: Option<GroupIp4>) -> NmP2pProvider<MockNm> {
        NmP2pProvider::new(MockNm {
            devices: vec![DEV.into()],
            peers: vec![peer()],
            states: Mutex::new(states.iter().copied().collect()),
            ip4,
            ..MockNm::default()
        })
    }

    fn provider() -> NmP2pProvider<MockNm> {
        provider_with(&[], Some(client_ip4()))
    }

    #[tokio::test]
    async fn no_p2p_device_is_a_readable_error() {
        let p = NmP2pProvider::new(MockNm::default());
        let err = p.start_find().await.unwrap_err();
        assert!(matches!(err, P2pError::NotAvailable(_)));
        assert!(err.to_string().contains("no Wi-Fi P2P device"));
    }

    #[tokio::test]
    async fn find_and_stop_use_the_first_device() {
        let p = provider();
        p.start_find().await.unwrap();
        p.stop_find().await.unwrap();
        assert_eq!(
            p.api.calls(),
            [format!("StartFind {DEV} 120s"), format!("StopFind {DEV}")]
        );
    }

    #[tokio::test]
    async fn set_wfd_ies_makes_no_nm_call() {
        let p = provider();
        p.set_wfd_ies(&[1, 2, 3]).await.unwrap();
        assert!(p.api.calls().is_empty());
    }

    #[tokio::test]
    async fn r1_flow_waits_while_activating_then_reads_client_group() {
        // NM stays "activating" until the user taps us on the tablet (R1), then activates.
        let p = provider_with(
            &[
                ActiveState::Unknown,
                ActiveState::Activating,
                ActiveState::Activating,
                ActiveState::Activated,
            ],
            Some(client_ip4()),
        );
        *p.api.ip4_delay.lock().unwrap() = 2;
        let ies = crate::wfd::sink_device_info(7236, 50);
        p.set_wfd_ies(&ies).await.unwrap();
        let g = p.connect(&peer().path, WpsMethod::Pbc).await.unwrap();
        assert_eq!(
            g,
            GroupInfo {
                interface: "p2p-wlp58s0-3".into(),
                role: GroupRole::Client,
                local_ip: Ipv4Addr::new(192, 168, 49, 74),
                peer_ip: Some(Ipv4Addr::new(192, 168, 49, 1)),
            }
        );
        let specs = p.api.specs();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].peer, "7A:1B:85:73:E1:76");
        assert_eq!(specs[0].wps_method_value(), 0x4);
        assert_eq!(specs[0].wfd_ies, ies, "R2: the IE travels in the profile");
        assert_eq!(
            p.api.calls(),
            [format!("AddAndActivate2 {DEV} peer=7A:1B:85:73:E1:76")]
        );
        // disconnect deactivates exactly the connection we started, once.
        p.disconnect().await.unwrap();
        p.disconnect().await.unwrap();
        assert_eq!(
            p.api.calls().last().unwrap(),
            &format!("Deactivate {MOCK_ACTIVE}")
        );
        assert_eq!(p.api.calls().len(), 2);
    }

    #[tokio::test]
    async fn vanished_connection_is_a_readable_failure_and_cleans_up() {
        let p = provider_with(&[ActiveState::Activating, ActiveState::Gone], None);
        let err = p.connect(&peer().path, WpsMethod::Pbc).await.unwrap_err();
        assert!(matches!(err, P2pError::Backend(_)), "{err}");
        assert!(err.to_string().contains("Android_qqQ0"), "{err}");
        assert!(err.to_string().contains("did not form"), "{err}");
        assert!(p.api.calls().contains(&format!("Deactivate {MOCK_ACTIVE}")));
        // Nothing left to disconnect.
        let n = p.api.calls().len();
        p.disconnect().await.unwrap();
        assert_eq!(p.api.calls().len(), n);
    }

    #[tokio::test]
    async fn no_tap_on_the_source_times_out_and_deactivates() {
        let p = provider_with(&[ActiveState::Activating], None)
            .with_group_timeout(Duration::from_secs(2));
        let err = p.connect(&peer().path, WpsMethod::Pbc).await.unwrap_err();
        assert!(matches!(err, P2pError::Timeout(_)), "{err}");
        assert!(err.to_string().contains("tap this device"), "{err}");
        assert!(p.api.calls().contains(&format!("Deactivate {MOCK_ACTIVE}")));
    }

    #[tokio::test]
    async fn activated_without_address_times_out() {
        let p = provider_with(&[], None);
        let err = p.connect(&peer().path, WpsMethod::Pbc).await.unwrap_err();
        assert!(err.to_string().contains("no IPv4 address"), "{err}");
    }

    #[tokio::test]
    async fn connect_to_unknown_peer_fails_without_touching_nm() {
        let p = provider();
        let err = p.connect("/nope", WpsMethod::Pbc).await.unwrap_err();
        assert!(matches!(err, P2pError::PeerNotFound(_)));
        assert!(p.api.specs().is_empty());
    }

    #[test]
    fn group_roles_from_ip4() {
        let g = group_from_ip4(&client_ip4()).unwrap();
        assert_eq!(g.role, GroupRole::Client);
        // We are GO (shared): no gateway, peer address unknown yet.
        let go = GroupIp4 {
            interface: "p2p-wlp58s0-0".into(),
            addresses: vec![(Ipv4Addr::new(10, 42, 0, 1), 24)],
            gateway: None,
        };
        let g = group_from_ip4(&go).unwrap();
        assert_eq!((g.role, g.peer_ip), (GroupRole::Owner, None));
        // A gateway equal to our own address or 0.0.0.0 is not a peer.
        for gw in [Ipv4Addr::new(10, 42, 0, 1), Ipv4Addr::UNSPECIFIED] {
            let g = group_from_ip4(&GroupIp4 {
                gateway: Some(gw),
                ..go.clone()
            })
            .unwrap();
            assert_eq!(g.role, GroupRole::Owner);
        }
        assert!(group_from_ip4(&GroupIp4 {
            addresses: vec![],
            ..go
        })
        .is_err());
    }

    #[test]
    fn active_state_values() {
        assert_eq!(ActiveState::from_nm(1), ActiveState::Activating);
        assert_eq!(ActiveState::from_nm(2), ActiveState::Activated);
        assert_eq!(ActiveState::from_nm(4), ActiveState::Deactivated);
        assert_eq!(ActiveState::from_nm(99), ActiveState::Unknown);
    }

    #[tokio::test]
    async fn events_pass_through() {
        let p = provider();
        p.api.push_event(P2pEvent::GroupEnded);
        assert_eq!(p.next_event().await, Some(P2pEvent::GroupEnded));
        assert_eq!(p.next_event().await, None);
    }
}
