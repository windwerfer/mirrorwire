//! MirrorWire viewer for Android.
//!
//! The APK is a thin Java shell (`packaging/android`, a `NativeActivity`
//! subclass that only holds a Wi-Fi multicast lock and forwards
//! `mirrorwire://` links) around this library. Everything else is Rust:
//! egui UI, QUIC session (`mirrorwire-net`), and H.264 decode through the
//! platform `MediaCodec` (hardware first, FilmCraft's pure-Rust decoder from
//! `mirrorwire-film` as the fallback).

// `unsafe` only in gpu.rs (EGL/GL FFI, AGENTS.md rule 12).
#![deny(unsafe_code)]

pub mod codec_list;
/// FilmCraft's software decoder; all of its API use lives in `mirrorwire-film`.
pub use mirrorwire_film as film;
pub mod h264;
pub mod stats;
pub mod yuv;

#[cfg(target_os = "android")]
mod app;
#[cfg(target_os = "android")]
mod chrome;
#[cfg(target_os = "android")]
mod codec;
#[cfg(target_os = "android")]
#[allow(unsafe_code)]
mod gpu;
#[cfg(target_os = "android")]
mod session;

/// Entry point called by android-activity's NativeActivity glue.
// `#[no_mangle]` counts as unsafe code for the lint; the NativeActivity glue needs the symbol.
#[cfg(target_os = "android")]
#[allow(unsafe_code)]
#[no_mangle]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    app::run(app);
}
