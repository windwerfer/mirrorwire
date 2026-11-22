// This crate never uses `unsafe` (AGENTS.md rule 12).
#![forbid(unsafe_code)]

pub mod backoff;
pub mod bottleneck;
pub mod clock;
pub mod control;
pub mod crc;
pub mod cursor;
pub mod delay;
pub mod fec;
pub mod fragment;
pub mod nack;
pub mod rate;
pub mod stats;

/// Presenter and viewer must speak the same version (checked in `hello`).
/// Bumped on every incompatible change; there is no compatibility with
/// other versions (decisions.md D6).
pub const PROTOCOL_VERSION: u32 = 11;
pub const ALPN: &[u8] = b"mirrorwire/1";
pub const MDNS_SERVICE: &str = "_mirrorwire._udp.local.";
pub const DEFAULT_PORT: u16 = 47_800;

/// `N` bytes at `offset` of `d`, or `None` if the slice is too short. All wire parsers
/// read through this (no indexing, no panics on hostile input).
pub(crate) fn be<const N: usize>(d: &[u8], offset: usize) -> Option<[u8; N]> {
    d.get(offset..offset.checked_add(N)?)?.try_into().ok()
}
