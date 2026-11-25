//! Camera frames for QR scanning (the desktop viewer's "Scan" button).
//!
//! Two parts: [`extract_luma`] turns one raw camera frame into a tightly packed 8-bit grey
//! image (pure, unit-tested, no feature needed), and `CameraScanner` (Linux, cargo feature
//! `portal`) asks the Camera portal for access, reads frames from the camera's PipeWire node
//! and runs a caller-supplied decoder (the app passes `pairing::scan_qr_luma`) on a worker
//! thread. Raw formats only (YUY2, UYVY, NV12, I420, GRAY8, BGRx, RGBx); an MJPEG-only camera
//! reports that in the scan status. No `unsafe`.

/// Raw pixel layouts the scanner can turn into luma.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LumaFormat {
    /// Packed Y0 U Y1 V.
    Yuy2,
    /// Packed U Y0 V Y1.
    Uyvy,
    /// Planar Y plane first (NV12 and I420 both start with the full-size Y plane).
    Planar420,
    Gray8,
    /// Bytes B G R x.
    Bgrx,
    /// Bytes R G B x.
    Rgbx,
}

/// Largest frame side accepted (pixels); bigger input is dropped unread.
pub const MAX_SIDE: usize = 8192;

/// Extracts the luma plane of one frame into a `width * height` buffer (no row padding).
/// `stride` is the byte distance between rows (0 = tightly packed). Returns `None` for bad
/// sizes or when `data` is too short.
pub fn extract_luma(
    format: LumaFormat,
    width: usize,
    height: usize,
    stride: usize,
    data: &[u8],
) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    let (bpp, y_offset, y_step) = match format {
        LumaFormat::Yuy2 => (2, 0, 2),
        LumaFormat::Uyvy => (2, 1, 2),
        LumaFormat::Planar420 | LumaFormat::Gray8 => (1, 0, 1),
        LumaFormat::Bgrx | LumaFormat::Rgbx => (4, 0, 0),
    };
    let row_bytes = width.checked_mul(bpp)?;
    let stride = if stride == 0 { row_bytes } else { stride };
    if stride < row_bytes {
        return None;
    }
    let mut out = Vec::with_capacity(width * height);
    for y in 0..height {
        let start = y.checked_mul(stride)?;
        let row = data.get(start..start.checked_add(row_bytes)?)?;
        match format {
            LumaFormat::Bgrx | LumaFormat::Rgbx => {
                let rgb = format == LumaFormat::Rgbx;
                out.extend(row.as_chunks::<4>().0.iter().map(|px| {
                    let (r, g, b) = match *px {
                        [r, g, b, _] if rgb => (r, g, b),
                        [b, g, r, _] => (r, g, b),
                    };
                    ((77 * r as u32 + 150 * g as u32 + 29 * b as u32) >> 8) as u8
                }));
            }
            _ if y_step == 1 => out.extend_from_slice(row),
            _ => out.extend(row.iter().skip(y_offset).step_by(y_step).copied()),
        }
    }
    Some(out)
}

#[cfg(all(target_os = "linux", feature = "portal"))]
pub use scanner::{CameraScanner, ScanEvent};

#[cfg(all(target_os = "linux", feature = "portal"))]
mod scanner {
    use super::{extract_luma, LumaFormat};
    use anyhow::{anyhow, Context, Result};
    use ashpd::desktop::camera::Camera;
    use crossbeam_channel::{bounded, unbounded, Receiver, Sender, TrySendError};
    use pipewire as pw;
    use pw::spa;
    use std::os::fd::OwnedFd;
    use std::time::{Duration, Instant};

    /// What a running scan reports to the UI.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ScanEvent<T> {
        /// Progress or the negotiated camera format, shown next to the Scan button.
        Status(String),
        /// The decoder found something.
        Found(T),
        /// The scan cannot continue (no camera, access denied, ...); shown in the window.
        Failed(String),
    }

    /// A running camera scan. Dropping it stops the camera.
    pub struct CameraScanner<T> {
        events: Receiver<ScanEvent<T>>,
        stop: pw::channel::Sender<()>,
    }

    impl<T: Send + 'static> CameraScanner<T> {
        /// Starts a scan. `decode(width, height, luma)` runs on a worker thread, at most a few
        /// times per second; the first `Some` ends the scan with [`ScanEvent::Found`].
        pub fn start<F>(decode: F) -> Self
        where
            F: Fn(usize, usize, &[u8]) -> Option<T> + Send + 'static,
        {
            let (events_tx, events) = unbounded();
            let (stop, stop_rx) = pw::channel::channel::<()>();
            let tx = events_tx.clone();
            let spawned = std::thread::Builder::new()
                .name("camera".into())
                .spawn(move || {
                    if let Err(e) = run(decode, events_tx, stop_rx) {
                        let _ = tx.send(ScanEvent::Failed(format!("{e:#}")));
                    }
                });
            if let Err(e) = spawned {
                tracing::warn!("camera thread: {e}");
            }
            Self { events, stop }
        }

        /// Next pending event, if any (non-blocking).
        pub fn poll(&self) -> Option<ScanEvent<T>> {
            self.events.try_recv().ok()
        }
    }

    impl<T> Drop for CameraScanner<T> {
        fn drop(&mut self) {
            let _ = self.stop.send(());
        }
    }

    fn run<T, F>(
        decode: F,
        events: Sender<ScanEvent<T>>,
        stop: pw::channel::Receiver<()>,
    ) -> Result<()>
    where
        T: Send + 'static,
        F: Fn(usize, usize, &[u8]) -> Option<T> + Send + 'static,
    {
        // The shared portal runtime: a runtime of our own would take ashpd's D-Bus
        // connection with it when this scan ends (see `portal_rt`).
        let rt = crate::source::portal_rt::portal_runtime()?;
        let _ = events.send(ScanEvent::Status("asking for camera access...".into()));
        let fd = rt.block_on(open_camera())?;
        let _ = events.send(ScanEvent::Status(
            "camera opened, looking for a code...".into(),
        ));

        // The decoder is slow next to the frame rate: it gets the newest frame only.
        let (luma_tx, luma_rx) = bounded::<(usize, usize, Vec<u8>)>(1);
        let worker_events = events.clone();
        let worker = std::thread::Builder::new()
            .name("qr-decode".into())
            .spawn(move || {
                while let Ok((w, h, luma)) = luma_rx.recv() {
                    if let Some(found) = decode(w, h, &luma) {
                        let _ = worker_events.send(ScanEvent::Found(found));
                        break;
                    }
                }
            })?;
        let result = run_pipewire(fd, luma_tx, &events, stop);
        let _ = worker.join();
        result
    }

    async fn open_camera() -> Result<OwnedFd> {
        let camera = Camera::new()
            .await
            .context("the Camera portal is not available")?;
        if !camera.is_present().await.unwrap_or(false) {
            return Err(anyhow!("no camera found"));
        }
        camera
            .request_access(Default::default())
            .await
            .context("camera access request failed")?
            .response()
            .map_err(|e| anyhow!("camera access was not granted ({e})"))?;
        camera
            .open_pipe_wire_remote(Default::default())
            .await
            .context("could not open the camera's PipeWire remote")
    }

    struct State<T> {
        format: spa::param::video::VideoInfoRaw,
        luma: Sender<(usize, usize, Vec<u8>)>,
        events: Sender<ScanEvent<T>>,
        last: Option<Instant>,
    }

    fn map_format(f: spa::param::video::VideoFormat) -> Option<LumaFormat> {
        use spa::param::video::VideoFormat as V;
        match f {
            V::YUY2 => Some(LumaFormat::Yuy2),
            V::UYVY => Some(LumaFormat::Uyvy),
            V::NV12 | V::I420 => Some(LumaFormat::Planar420),
            V::GRAY8 => Some(LumaFormat::Gray8),
            V::BGRx | V::BGRA => Some(LumaFormat::Bgrx),
            V::RGBx | V::RGBA => Some(LumaFormat::Rgbx),
            _ => None,
        }
    }

    fn format_pod() -> Result<Vec<u8>> {
        use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
        use spa::param::video::VideoFormat as V;
        use spa::utils::{Rectangle, SpaTypes};
        let obj = spa::pod::object!(
            SpaTypes::ObjectParamFormat,
            spa::param::ParamType::EnumFormat,
            spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
            spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
            spa::pod::property!(
                FormatProperties::VideoFormat,
                Choice,
                Enum,
                Id,
                V::YUY2,
                V::YUY2,
                V::NV12,
                V::I420,
                V::UYVY,
                V::GRAY8,
                V::BGRx,
                V::RGBx
            ),
            spa::pod::property!(
                FormatProperties::VideoSize,
                Choice,
                Range,
                Rectangle,
                Rectangle {
                    width: 1280,
                    height: 720
                },
                Rectangle {
                    width: 160,
                    height: 120
                },
                Rectangle {
                    width: 1920,
                    height: 1080
                }
            ),
        );
        let (cursor, _) = spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &spa::pod::Value::Object(obj),
        )
        .map_err(|e| anyhow!("serialize camera format: {e:?}"))?;
        Ok(cursor.into_inner())
    }

    fn run_pipewire<T: Send + 'static>(
        fd: OwnedFd,
        luma: Sender<(usize, usize, Vec<u8>)>,
        events: &Sender<ScanEvent<T>>,
        stop: pw::channel::Receiver<()>,
    ) -> Result<()> {
        pw::init();
        let mainloop = pw::main_loop::MainLoopRc::new(None)?;
        let context = pw::context::ContextRc::new(&mainloop, None)?;
        let core = context.connect_fd_rc(fd, None)?;
        let stream = pw::stream::StreamBox::new(
            &core,
            "mirrorwire-qr-scan",
            pw::properties::properties! {
                *pw::keys::MEDIA_TYPE => "Video",
                *pw::keys::MEDIA_CATEGORY => "Capture",
                *pw::keys::MEDIA_ROLE => "Camera",
            },
        )?;
        let state = State {
            format: Default::default(),
            luma,
            events: events.clone(),
            last: None,
        };
        let _listener = stream
            .add_local_listener_with_user_data(state)
            .param_changed(|_, state, id, param| {
                let Some(param) = param else { return };
                if id != spa::param::ParamType::Format.as_raw() {
                    return;
                }
                if state.format.parse(param).is_err() {
                    return;
                }
                let size = state.format.size();
                let text = if map_format(state.format.format()).is_some() {
                    format!(
                        "camera {:?} {}x{}, looking for a code...",
                        state.format.format(),
                        size.width,
                        size.height
                    )
                } else {
                    format!(
                        "this camera sends {:?}, which the scanner cannot read",
                        state.format.format()
                    )
                };
                let _ = state.events.send(ScanEvent::Status(text));
            })
            .process(|stream, state| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                // The decoder takes a few frames a second; skip the rest unread.
                if state
                    .last
                    .is_some_and(|t| t.elapsed() < Duration::from_millis(150))
                {
                    return;
                }
                let Some(format) = map_format(state.format.format()) else {
                    return;
                };
                let size = state.format.size();
                let Some(data) = buffer.datas_mut().first_mut() else {
                    return;
                };
                let chunk = data.chunk();
                let (offset, len, stride) = (
                    chunk.offset() as usize,
                    chunk.size() as usize,
                    chunk.stride().max(0) as usize,
                );
                let Some(bytes) = data
                    .data()
                    .and_then(|m| m.get(offset..offset.checked_add(len)?))
                else {
                    return;
                };
                let (w, h) = (size.width as usize, size.height as usize);
                if let Some(plane) = extract_luma(format, w, h, stride, bytes) {
                    state.last = Some(Instant::now());
                    if let Err(TrySendError::Disconnected(_)) = state.luma.try_send((w, h, plane)) {
                        // Worker finished (code found): nothing more to do.
                    }
                }
            })
            .register()?;

        let pod = format_pod()?;
        let mut params = [spa::pod::Pod::from_bytes(&pod).ok_or_else(|| anyhow!("bad pod"))?];
        // No node id: the portal's PipeWire remote only exposes the cameras.
        stream.connect(
            spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::DONT_RECONNECT,
            &mut params,
        )?;
        let quit = mainloop.clone();
        let _stop = stop.attach(mainloop.loop_(), move |_| quit.quit());
        mainloop.run();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yuy2_and_uyvy_pick_the_y_bytes() {
        // 4x2, stride 10 (2 bytes padding per row).
        let mut yuy2 = vec![0u8; 20];
        let mut uyvy = vec![0u8; 20];
        for y in 0..2 {
            for x in 0..4 {
                let v = (y * 4 + x + 1) as u8 * 10;
                yuy2[y * 10 + x * 2] = v;
                yuy2[y * 10 + x * 2 + 1] = 99;
                uyvy[y * 10 + x * 2 + 1] = v;
                uyvy[y * 10 + x * 2] = 99;
            }
        }
        let want = vec![10, 20, 30, 40, 50, 60, 70, 80];
        assert_eq!(
            extract_luma(LumaFormat::Yuy2, 4, 2, 10, &yuy2),
            Some(want.clone())
        );
        assert_eq!(extract_luma(LumaFormat::Uyvy, 4, 2, 10, &uyvy), Some(want));
    }

    #[test]
    fn planar_and_gray_copy_rows_and_ignore_chroma() {
        let mut nv12 = [0u8; 6 * 3]; // 4x2 Y with stride 6, then chroma
        nv12[0..4].copy_from_slice(&[1, 2, 3, 4]);
        nv12[6..10].copy_from_slice(&[5, 6, 7, 8]);
        // Chroma starts after height*stride; data is only checked to cover the Y rows.
        let got = extract_luma(LumaFormat::Planar420, 4, 2, 6, &nv12[..12]).unwrap();
        assert_eq!(got, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            extract_luma(LumaFormat::Gray8, 2, 2, 0, &[9, 8, 7, 6]),
            Some(vec![9, 8, 7, 6])
        );
    }

    #[test]
    fn rgb_formats_weigh_green_most() {
        // One pixel: white, black, pure green, pure red/blue by layout.
        let bgrx = [255, 255, 255, 0, 0, 0, 0, 0];
        assert_eq!(
            extract_luma(LumaFormat::Bgrx, 2, 1, 0, &bgrx),
            Some(vec![255, 0])
        );
        let green = extract_luma(LumaFormat::Bgrx, 1, 1, 0, &[0, 255, 0, 0]).unwrap()[0];
        let red_bgrx = extract_luma(LumaFormat::Bgrx, 1, 1, 0, &[0, 0, 255, 0]).unwrap()[0];
        let red_rgbx = extract_luma(LumaFormat::Rgbx, 1, 1, 0, &[255, 0, 0, 0]).unwrap()[0];
        assert!(green > red_bgrx);
        assert_eq!(red_bgrx, red_rgbx);
    }

    #[test]
    fn hostile_sizes_are_rejected() {
        assert_eq!(extract_luma(LumaFormat::Gray8, 0, 4, 0, &[]), None);
        assert_eq!(extract_luma(LumaFormat::Gray8, 4, 4, 0, &[0; 15]), None);
        assert_eq!(extract_luma(LumaFormat::Gray8, 4, 4, 3, &[0; 64]), None);
        assert_eq!(extract_luma(LumaFormat::Yuy2, usize::MAX, 2, 0, &[]), None);
        assert_eq!(
            extract_luma(LumaFormat::Gray8, MAX_SIDE + 1, 1, 0, &[]),
            None
        );
        assert_eq!(
            extract_luma(LumaFormat::Gray8, 2, 2, usize::MAX, &[0; 8]),
            None
        );
    }
}
