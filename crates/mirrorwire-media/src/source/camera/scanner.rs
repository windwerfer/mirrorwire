//! The PipeWire camera scanner behind [`super::CameraScanner`]. Needs the Camera portal and a
//! camera, so it only runs on a desktop session; the pure frame conversion stays in `camera.rs`.

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

fn run<T, F>(decode: F, events: Sender<ScanEvent<T>>, stop: pw::channel::Receiver<()>) -> Result<()>
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
