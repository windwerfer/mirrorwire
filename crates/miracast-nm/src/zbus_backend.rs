//! NetworkManager over the system D-Bus (zbus). No root: every call goes through NM, which
//! asks polkit (miracast.md section 1). Discovery, peer listing, `AddAndActivateConnection2`
//! of a volatile profile, state and IPv4 reads and deactivation are real calls (the flow of
//! hardware runs 1-3, R1/R2 in miracast.md); change signals are not subscribed yet (the
//! provider polls). Everything NM-independent is tested through [`crate::MockNm`]; the
//! value parsing here is pure and unit tested.

use crate::nm::{ActiveState, GroupIp4};
use crate::{BoxFuture, NmApi, NmPeer, P2pConnectionSpec, P2pError, P2pEvent};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::Duration;
use zbus::{ObjectPath, OwnedObjectPath, OwnedVariant, Variant as Value};

const NM_DEST: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_IFACE: &str = "org.freedesktop.NetworkManager";
const DEVICE_IFACE: &str = "org.freedesktop.NetworkManager.Device";
const P2P_DEVICE_IFACE: &str = "org.freedesktop.NetworkManager.Device.WifiP2P";
const PEER_IFACE: &str = "org.freedesktop.NetworkManager.WifiP2PPeer";
const ACTIVE_IFACE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const IP4_IFACE: &str = "org.freedesktop.NetworkManager.IP4Config";
/// Bound on what we read from NM (AGENTS.md 1.9; NM is trusted, but cheap to bound).
const MAX_ADDRESSES: usize = 16;
const WIRELESS_IFACE: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const AP_IFACE: &str = "org.freedesktop.NetworkManager.AccessPoint";
/// `NM_DEVICE_TYPE_WIFI`.
const DEVICE_TYPE_WIFI: u32 = 2;
/// `NM_DEVICE_TYPE_WIFI_P2P`.
const DEVICE_TYPE_WIFI_P2P: u32 = 30;

/// `a{sa{sv}}`: NM connection settings.
pub type ConnectionSettings = HashMap<&'static str, HashMap<&'static str, Value<'static>>>;

/// Settings of the volatile `wifi-p2p` profile for `spec` (pure; unit tested).
pub fn connection_settings(spec: &P2pConnectionSpec) -> ConnectionSettings {
    let mut connection = HashMap::new();
    connection.insert("id", Value::from(spec.id.clone()));
    connection.insert("type", Value::from("wifi-p2p"));
    connection.insert("autoconnect", Value::from(false));

    let mut p2p = HashMap::new();
    p2p.insert("peer", Value::from(spec.peer.clone()));
    p2p.insert("wps-method", Value::from(spec.wps_method_value()));
    if !spec.wfd_ies.is_empty() {
        p2p.insert("wfd-ies", Value::from(spec.wfd_ies.clone()));
    }

    // The source becomes group owner and serves DHCP (hardware runs 1-3: we got
    // 192.168.49.74 by DHCP from the tablet at 192.168.49.1), so we are a DHCP client.
    let mut ipv4 = HashMap::new();
    ipv4.insert("method", Value::from("auto"));

    let mut out = HashMap::new();
    out.insert("connection", connection);
    out.insert("wifi-p2p", p2p);
    out.insert("ipv4", ipv4);
    out
}

/// Options of `AddAndActivateConnection2`: the profile lives only while it is active.
pub fn activation_options() -> HashMap<&'static str, Value<'static>> {
    let mut o = HashMap::new();
    o.insert("persist", Value::from("volatile"));
    o
}

/// `IP4Config.AddressData` entries (`address` s, `prefix` u) to addresses; malformed entries
/// are skipped.
pub fn parse_address_data(data: &[HashMap<String, OwnedVariant>]) -> Vec<(Ipv4Addr, u8)> {
    data.iter()
        .take(MAX_ADDRESSES)
        .filter_map(|e| {
            let addr: &str = e.get("address").and_then(|v| <&str>::try_from(&**v).ok())?;
            let prefix = e.get("prefix").and_then(|v| u32::try_from(&**v).ok())?;
            Some((
                addr.parse().ok()?,
                u8::try_from(prefix).ok().filter(|p| *p <= 32)?,
            ))
        })
        .collect()
}

/// `IP4Config.Gateway` (empty when there is none).
pub fn parse_gateway(s: &str) -> Option<Ipv4Addr> {
    s.parse().ok().filter(|g: &Ipv4Addr| !g.is_unspecified())
}

fn is_unknown_object(e: &zbus::Error) -> bool {
    let t = e.to_string();
    t.contains("UnknownObject") || t.contains("UnknownMethod") || t.contains("does not exist")
}

fn map_err(e: zbus::Error) -> P2pError {
    let text = e.to_string();
    if text.contains("NotAuthorized") || text.contains("AccessDenied") {
        P2pError::PermissionDenied(text)
    } else if text.contains("ServiceUnknown") || text.contains("NameHasNoOwner") {
        P2pError::NotAvailable(format!("NetworkManager is not running ({text})"))
    } else {
        P2pError::Backend(text)
    }
}

/// [`NmApi`] on the system bus.
pub struct ZbusNm {
    conn: zbus::Connection,
}

impl ZbusNm {
    /// Connect to the system bus. Long method timeout: polkit may show a password prompt.
    pub async fn connect_system() -> Result<Self, P2pError> {
        let conn = zbus::connection::Builder::system()
            .method_timeout(Duration::from_secs(120))
            .build()
            .await
            .map_err(map_err)?;
        Ok(Self { conn })
    }

    async fn proxy<'a>(
        &'a self,
        path: &'a str,
        interface: &'a str,
    ) -> Result<zbus::Proxy<'a>, P2pError> {
        zbus::proxy::Builder::new(&self.conn)
            .destination(NM_DEST)
            .path(path)
            .interface(interface)
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .map_err(map_err)
    }

    /// Frequency (MHz) of each Wi-Fi station's current access point, e.g.
    /// `[("wlp58s0", 2462)]`. The P2P group shares that radio; NM does not expose the group's
    /// own frequency (miracast.md section 12), so this is logged next to each session.
    pub async fn sta_frequencies(&self) -> Result<Vec<(String, u32)>, P2pError> {
        let nm = self.proxy(NM_PATH, NM_IFACE).await?;
        let all: Vec<OwnedObjectPath> = nm.call("GetAllDevices", &()).await.map_err(map_err)?;
        let mut out = Vec::new();
        for path in all {
            let dev = self.proxy(path.as_str(), DEVICE_IFACE).await?;
            let ty: u32 = dev.get_property("DeviceType").await.map_err(map_err)?;
            if ty != DEVICE_TYPE_WIFI {
                continue;
            }
            let iface: String = dev.get_property("Interface").await.map_err(map_err)?;
            let wifi = self.proxy(path.as_str(), WIRELESS_IFACE).await?;
            let ap: OwnedObjectPath = wifi
                .get_property("ActiveAccessPoint")
                .await
                .map_err(map_err)?;
            if ap.as_str() == "/" {
                continue;
            }
            let ap = self.proxy(ap.as_str(), AP_IFACE).await?;
            // The AP can vanish between the two reads (roaming); skip it then.
            if let Ok(freq) = ap.get_property::<u32>("Frequency").await {
                out.push((iface, freq));
            }
        }
        Ok(out)
    }

    async fn peer(&self, path: &str) -> Result<NmPeer, P2pError> {
        let p = self.proxy(path, PEER_IFACE).await?;
        Ok(NmPeer {
            path: path.to_string(),
            name: p.get_property("Name").await.map_err(map_err)?,
            hw_address: p.get_property("HwAddress").await.map_err(map_err)?,
            wfd_ies: p.get_property("WfdIEs").await.unwrap_or_default(),
            strength: p.get_property("Strength").await.unwrap_or(0),
        })
    }
}

impl NmApi for ZbusNm {
    fn p2p_devices(&self) -> BoxFuture<'_, Result<Vec<String>, P2pError>> {
        Box::pin(async move {
            let nm = self.proxy(NM_PATH, NM_IFACE).await?;
            let all: Vec<OwnedObjectPath> = nm.call("GetAllDevices", &()).await.map_err(map_err)?;
            let mut out = Vec::new();
            for path in all {
                let dev = self.proxy(path.as_str(), DEVICE_IFACE).await?;
                let ty: u32 = dev.get_property("DeviceType").await.map_err(map_err)?;
                if ty == DEVICE_TYPE_WIFI_P2P {
                    out.push(path.to_string());
                }
            }
            Ok(out)
        })
    }

    fn start_find<'a>(
        &'a self,
        device: &'a str,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<(), P2pError>> {
        Box::pin(async move {
            let dev = self.proxy(device, P2P_DEVICE_IFACE).await?;
            let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
            let secs = i32::try_from(timeout.as_secs()).unwrap_or(i32::MAX);
            opts.insert("timeout", Value::from(secs));
            dev.call::<_, _, ()>("StartFind", &(opts,))
                .await
                .map_err(map_err)
        })
    }

    fn stop_find<'a>(&'a self, device: &'a str) -> BoxFuture<'a, Result<(), P2pError>> {
        Box::pin(async move {
            let dev = self.proxy(device, P2P_DEVICE_IFACE).await?;
            dev.call::<_, _, ()>("StopFind", &()).await.map_err(map_err)
        })
    }

    fn peers<'a>(&'a self, device: &'a str) -> BoxFuture<'a, Result<Vec<NmPeer>, P2pError>> {
        Box::pin(async move {
            let dev = self.proxy(device, P2P_DEVICE_IFACE).await?;
            let paths: Vec<OwnedObjectPath> = dev.get_property("Peers").await.map_err(map_err)?;
            let mut out = Vec::new();
            for p in paths {
                // A peer can vanish between listing and reading; skip it.
                if let Ok(peer) = self.peer(p.as_str()).await {
                    out.push(peer);
                }
            }
            Ok(out)
        })
    }

    fn activate<'a>(
        &'a self,
        device: &'a str,
        spec: &'a P2pConnectionSpec,
    ) -> BoxFuture<'a, Result<String, P2pError>> {
        Box::pin(async move {
            let nm = self.proxy(NM_PATH, NM_IFACE).await?;
            let dev = ObjectPath::try_from(device)
                .map_err(|e| P2pError::Backend(format!("bad device path: {e}")))?;
            let any = ObjectPath::from_static_str_unchecked("/");
            let (_conn, active, _result): (
                OwnedObjectPath,
                OwnedObjectPath,
                HashMap<String, OwnedVariant>,
            ) = nm
                .call(
                    "AddAndActivateConnection2",
                    &(connection_settings(spec), dev, any, activation_options()),
                )
                .await
                .map_err(map_err)?;
            Ok(active.to_string())
        })
    }

    fn active_state<'a>(&'a self, active: &'a str) -> BoxFuture<'a, Result<ActiveState, P2pError>> {
        Box::pin(async move {
            let ac = self.proxy(active, ACTIVE_IFACE).await?;
            match ac.get_property::<u32>("State").await {
                Ok(v) => Ok(ActiveState::from_nm(v)),
                // A failed volatile connection is removed at once (hardware run 1).
                Err(e) if is_unknown_object(&e) => Ok(ActiveState::Gone),
                Err(e) => Err(map_err(e)),
            }
        })
    }

    fn group_ip4<'a>(
        &'a self,
        active: &'a str,
    ) -> BoxFuture<'a, Result<Option<GroupIp4>, P2pError>> {
        Box::pin(async move {
            let ac = self.proxy(active, ACTIVE_IFACE).await?;
            let ip4: OwnedObjectPath = ac.get_property("Ip4Config").await.map_err(map_err)?;
            let devices: Vec<OwnedObjectPath> =
                ac.get_property("Devices").await.map_err(map_err)?;
            let Some(dev) = devices.first() else {
                return Ok(None);
            };
            if ip4.as_str() == "/" {
                return Ok(None);
            }
            let dev = self.proxy(dev.as_str(), DEVICE_IFACE).await?;
            let interface: String = dev.get_property("IpInterface").await.map_err(map_err)?;
            let cfg = self.proxy(ip4.as_str(), IP4_IFACE).await?;
            let data: Vec<HashMap<String, OwnedVariant>> =
                cfg.get_property("AddressData").await.map_err(map_err)?;
            let gateway: String = cfg.get_property("Gateway").await.unwrap_or_default();
            Ok(Some(GroupIp4 {
                interface,
                addresses: parse_address_data(&data),
                gateway: parse_gateway(&gateway),
            }))
        })
    }

    fn deactivate<'a>(&'a self, active: &'a str) -> BoxFuture<'a, Result<(), P2pError>> {
        Box::pin(async move {
            let nm = self.proxy(NM_PATH, NM_IFACE).await?;
            let path = ObjectPath::try_from(active)
                .map_err(|e| P2pError::Backend(format!("bad connection path: {e}")))?;
            match nm.call::<_, _, ()>("DeactivateConnection", &(path,)).await {
                Ok(()) => Ok(()),
                // Already gone (volatile profiles vanish when they fail).
                Err(e)
                    if is_unknown_object(&e) || e.to_string().contains("ConnectionNotActive") =>
                {
                    Ok(())
                }
                Err(e) => Err(map_err(e)),
            }
        })
    }

    fn sleep(&self, d: Duration) -> BoxFuture<'_, ()> {
        Box::pin(tokio::time::sleep(d))
    }

    fn next_event(&self) -> BoxFuture<'_, Option<P2pEvent>> {
        // Stub: PeerAdded/PeerRemoved and state-change signals are not subscribed yet.
        Box::pin(std::future::pending())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WpsMethod;

    #[test]
    fn settings_carry_peer_wps_and_ies() {
        let spec = P2pConnectionSpec {
            id: "x".into(),
            peer: "02:11:22:33:44:55".into(),
            wps: WpsMethod::Pbc,
            wfd_ies: vec![0, 0, 6, 0, 0x11, 0x1c, 0x44, 0, 0x32],
        };
        let s = connection_settings(&spec);
        assert_eq!(s["connection"]["type"], Value::from("wifi-p2p"));
        assert_eq!(s["connection"]["autoconnect"], Value::from(false));
        assert_eq!(s["wifi-p2p"]["peer"], Value::from("02:11:22:33:44:55"));
        assert_eq!(s["wifi-p2p"]["wps-method"], Value::from(4u32));
        assert!(s["wifi-p2p"].contains_key("wfd-ies"));
    }

    #[test]
    fn empty_ies_are_omitted() {
        let spec = P2pConnectionSpec {
            id: "x".into(),
            peer: "p".into(),
            wps: WpsMethod::Auto,
            wfd_ies: vec![],
        };
        assert!(!connection_settings(&spec)["wifi-p2p"].contains_key("wfd-ies"));
    }

    #[test]
    fn ipv4_is_auto_and_activation_is_volatile() {
        let spec = P2pConnectionSpec {
            id: "x".into(),
            peer: "p".into(),
            wps: WpsMethod::Pbc,
            wfd_ies: vec![],
        };
        assert_eq!(
            connection_settings(&spec)["ipv4"]["method"],
            Value::from("auto")
        );
        assert_eq!(activation_options()["persist"], Value::from("volatile"));
    }

    #[test]
    fn address_data_and_gateway_parse_hostile_values() {
        let entry = |addr: Value<'static>, prefix: Value<'static>| {
            let mut m = HashMap::new();
            m.insert("address".to_string(), OwnedVariant::try_from(addr).unwrap());
            m.insert(
                "prefix".to_string(),
                OwnedVariant::try_from(prefix).unwrap(),
            );
            m
        };
        let data = vec![
            entry(Value::from("192.168.49.74"), Value::from(24u32)),
            entry(Value::from("not an ip"), Value::from(24u32)),
            entry(Value::from("10.0.0.1"), Value::from(99u32)),
            entry(Value::from(5u32), Value::from(24u32)),
            HashMap::new(),
        ];
        assert_eq!(
            parse_address_data(&data),
            [(Ipv4Addr::new(192, 168, 49, 74), 24)]
        );
        assert_eq!(
            parse_gateway("192.168.49.1"),
            Some(Ipv4Addr::new(192, 168, 49, 1))
        );
        assert_eq!(parse_gateway(""), None);
        assert_eq!(parse_gateway("0.0.0.0"), None);
    }

    #[test]
    fn polkit_refusal_is_a_permission_error() {
        let e = map_err(zbus::Error::Failure(
            "org.freedesktop.NetworkManager.PermissionDenied".into(),
        ));
        assert!(matches!(
            e,
            P2pError::Backend(_) | P2pError::PermissionDenied(_)
        ));
    }
}
