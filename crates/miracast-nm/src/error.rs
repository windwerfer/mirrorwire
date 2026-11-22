use std::fmt;

/// Failure of a P2P operation. Messages are meant to be shown to the user as they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum P2pError {
    /// No Wi-Fi P2P capable device (no NetworkManager, no P2P in the supplicant or driver).
    NotAvailable(String),
    /// Polkit or the bus policy refused the call.
    PermissionDenied(String),
    /// The peer is not (or no longer) known.
    PeerNotFound(String),
    /// Group formation or a bus call did not finish in time.
    Timeout(String),
    /// The backend does not implement this yet (spike stub).
    Unsupported(String),
    /// Any other backend failure.
    Backend(String),
}

impl fmt::Display for P2pError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAvailable(m) => write!(f, "Wi-Fi Direct is not available: {m}"),
            Self::PermissionDenied(m) => write!(f, "Wi-Fi Direct was refused (permission): {m}"),
            Self::PeerNotFound(m) => write!(f, "Wi-Fi Direct peer not found: {m}"),
            Self::Timeout(m) => write!(f, "Wi-Fi Direct timed out: {m}"),
            Self::Unsupported(m) => write!(f, "Wi-Fi Direct step not implemented: {m}"),
            Self::Backend(m) => write!(f, "Wi-Fi Direct error: {m}"),
        }
    }
}

impl std::error::Error for P2pError {}
