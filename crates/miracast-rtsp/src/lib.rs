#![forbid(unsafe_code)]
//! Miracast sink-side RTSP (messages M1-M7, keepalive, teardown), sans-IO.
//!
//! The caller feeds bytes read from the TCP connection to [`SinkSession::handle_input`] and
//! the current time to [`SinkSession::poll`]; the machine returns [`Action`]s (bytes to send,
//! media to start/stop, close with a reason). See `docs/agents/miracast.md` section 3.

pub mod message;
pub mod params;
mod session;

pub use session::{Action, Negotiated, SinkConfig, SinkSession, State};
