use crate::{BoxFuture, GroupInfo, P2pError, P2pEvent, P2pProvider, PeerInfo, WpsMethod};
use std::collections::VecDeque;
use std::sync::Mutex;

/// What the mock saw, in order, for assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockCall {
    SetWfdIes(Vec<u8>),
    StartFind,
    StopFind,
    Connect(String, WpsMethod),
    Disconnect,
}

#[derive(Default)]
struct State {
    calls: Vec<MockCall>,
    peers: Vec<PeerInfo>,
    events: VecDeque<P2pEvent>,
    group: Option<GroupInfo>,
    /// Number of upcoming `connect` calls that fail like a group formation failure.
    failing_connects: usize,
}

/// Scriptable in-memory provider. `connect` returns the configured group; events queued with
/// [`push_event`](Self::push_event) come out of `next_event` (then `None`).
#[derive(Default)]
pub struct MockP2pProvider {
    state: Mutex<State>,
}

impl MockP2pProvider {
    pub fn new() -> Self {
        Self::default()
    }

    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        // A poisoned mutex only means a test thread panicked; the data is still usable.
        let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut g)
    }

    pub fn add_peer(&self, peer: PeerInfo) {
        self.with(|s| s.peers.push(peer));
    }

    pub fn set_group(&self, group: GroupInfo) {
        self.with(|s| s.group = Some(group));
    }

    /// The next `n` connects fail with a backend error (group formation failed).
    pub fn fail_connects(&self, n: usize) {
        self.with(|s| s.failing_connects = n);
    }

    pub fn push_event(&self, event: P2pEvent) {
        self.with(|s| s.events.push_back(event));
    }

    pub fn calls(&self) -> Vec<MockCall> {
        self.with(|s| s.calls.clone())
    }
}

impl P2pProvider for MockP2pProvider {
    fn set_wfd_ies<'a>(&'a self, ies: &'a [u8]) -> BoxFuture<'a, Result<(), P2pError>> {
        self.with(|s| s.calls.push(MockCall::SetWfdIes(ies.to_vec())));
        Box::pin(async { Ok(()) })
    }

    fn start_find(&self) -> BoxFuture<'_, Result<(), P2pError>> {
        self.with(|s| s.calls.push(MockCall::StartFind));
        Box::pin(async { Ok(()) })
    }

    fn stop_find(&self) -> BoxFuture<'_, Result<(), P2pError>> {
        self.with(|s| s.calls.push(MockCall::StopFind));
        Box::pin(async { Ok(()) })
    }

    fn peers(&self) -> BoxFuture<'_, Result<Vec<PeerInfo>, P2pError>> {
        let peers = self.with(|s| s.peers.clone());
        Box::pin(async { Ok(peers) })
    }

    fn connect<'a>(
        &'a self,
        peer_id: &'a str,
        wps: WpsMethod,
    ) -> BoxFuture<'a, Result<GroupInfo, P2pError>> {
        let result = self.with(|s| {
            s.calls.push(MockCall::Connect(peer_id.to_string(), wps));
            if !s.peers.iter().any(|p| p.id == peer_id) {
                return Err(P2pError::PeerNotFound(peer_id.to_string()));
            }
            if s.failing_connects > 0 {
                s.failing_connects -= 1;
                return Err(P2pError::Backend("mock: group formation failed".into()));
            }
            s.group
                .clone()
                .ok_or_else(|| P2pError::Backend("mock has no group configured".into()))
        });
        Box::pin(async { result })
    }

    fn disconnect(&self) -> BoxFuture<'_, Result<(), P2pError>> {
        self.with(|s| s.calls.push(MockCall::Disconnect));
        Box::pin(async { Ok(()) })
    }

    fn next_event(&self) -> BoxFuture<'_, Option<P2pEvent>> {
        let ev = self.with(|s| s.events.pop_front());
        Box::pin(async { ev })
    }
}
