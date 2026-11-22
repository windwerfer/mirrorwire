//! The virtual pen: a uinput tablet with pressure (decisions D26), built from the kernel's
//! documented uinput API (`input/uinput.rst`) and event codes (`input/event-codes.rst`):
//! `BTN_TOOL_PEN` while the pen is near, `BTN_TOUCH` while it touches, `ABS_X`/`ABS_Y` for
//! the position and `ABS_PRESSURE`. `INPUT_PROP_DIRECT` says the coordinates map straight onto
//! the screen (a display tablet), which is what the viewer's picture is; `INPUT_PROP_POINTER`
//! (an off-screen tablet that needs a cursor) is not set.
//!
//! The ioctls are done by the `evdev` crate (Apache-2.0 OR MIT), so this file has no `unsafe`
//! (D15). The device exists while [`UinputPen`] lives; dropping it closes the fd, which makes
//! the kernel remove the device.

use super::PenDevice;
use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode,
    PropType, UinputAbsSetup,
};
use mirrorwire_proto::control::PointerPhase;
use std::io;

/// Shown in the desktop's tablet settings (KDE: System Settings → Drawing Tablet).
pub const DEVICE_NAME: &str = "MirrorWire virtual pen";
/// Position range: the wire's 0..=65535 inside the picture, unchanged.
const XY_MAX: i32 = 65_535;
/// Nominal resolution (units per mm, about a 33 cm wide surface). Tablets should report one;
/// the size has no other meaning here.
const XY_RES: i32 = 200;
/// Pressure range, a common tablet value; arbitrary units (resolution 0, event-codes.rst).
pub const PRESSURE_MAX: i32 = 4095;

/// One evdev event as (type, code, value), so the sequences are testable without a device.
pub type Ev = (u16, u16, i32);
/// Events of one `SYN_REPORT` frame.
pub type Frame = Vec<Ev>;

const KEY: u16 = EventType::KEY.0;
const ABS: u16 = EventType::ABSOLUTE.0;

/// Pen pressure on the device scale. No reading (mouse, finger without force) is full
/// pressure, like a mouse click; while touching it is at least 1 so a touch never reads as
/// "not pressed".
pub fn device_pressure(pressure: Option<u16>) -> i32 {
    match pressure {
        None => PRESSURE_MAX,
        Some(p) => (i32::from(p) * PRESSURE_MAX / 65_535).max(1),
    }
}

/// The frames for one pointer event, given whether the pen touches now. Returns the frames
/// and whether it touches afterwards. Down: proximity in at the position, then the touch
/// (`BTN_TOUCH` first in its frame, event-codes.rst). Move: position and pressure (ignored
/// when not touching: the viewer only sends moves between down and up). Up: touch off, then
/// proximity out.
pub fn frames(
    touching: bool,
    phase: PointerPhase,
    x: u16,
    y: u16,
    pressure: Option<u16>,
) -> (Vec<Frame>, bool) {
    let (x, y) = (i32::from(x), i32::from(y));
    let pos = |v: &mut Frame| {
        v.push((ABS, AbsoluteAxisCode::ABS_X.0, x));
        v.push((ABS, AbsoluteAxisCode::ABS_Y.0, y));
    };
    match phase {
        PointerPhase::Down => {
            let mut out = Vec::new();
            if !touching {
                let mut near = vec![(KEY, KeyCode::BTN_TOOL_PEN.0, 1)];
                pos(&mut near);
                near.push((ABS, AbsoluteAxisCode::ABS_PRESSURE.0, 0));
                out.push(near);
            }
            let mut touch = vec![(KEY, KeyCode::BTN_TOUCH.0, 1)];
            pos(&mut touch);
            touch.push((
                ABS,
                AbsoluteAxisCode::ABS_PRESSURE.0,
                device_pressure(pressure),
            ));
            out.push(touch);
            (out, true)
        }
        PointerPhase::Move if touching => {
            let mut f = Vec::new();
            pos(&mut f);
            f.push((
                ABS,
                AbsoluteAxisCode::ABS_PRESSURE.0,
                device_pressure(pressure),
            ));
            (vec![f], true)
        }
        PointerPhase::Move => (Vec::new(), false),
        PointerPhase::Up if touching => (lift(Some((x, y))), false),
        PointerPhase::Up => (Vec::new(), false),
    }
}

/// Touch off (at `pos` if given), then proximity out.
fn lift(pos: Option<(i32, i32)>) -> Vec<Frame> {
    let mut up = vec![(KEY, KeyCode::BTN_TOUCH.0, 0)];
    if let Some((x, y)) = pos {
        up.push((ABS, AbsoluteAxisCode::ABS_X.0, x));
        up.push((ABS, AbsoluteAxisCode::ABS_Y.0, y));
    }
    up.push((ABS, AbsoluteAxisCode::ABS_PRESSURE.0, 0));
    vec![up, vec![(KEY, KeyCode::BTN_TOOL_PEN.0, 0)]]
}

/// A live virtual pen.
pub struct UinputPen {
    device: VirtualDevice,
    touching: bool,
}

impl UinputPen {
    /// Create the device. Fails with the OS error when `/dev/uinput` cannot be opened.
    pub fn create() -> io::Result<Self> {
        let keys: AttributeSet<KeyCode> = [KeyCode::BTN_TOOL_PEN, KeyCode::BTN_TOUCH]
            .into_iter()
            .collect();
        let props: AttributeSet<PropType> = [PropType::DIRECT].into_iter().collect();
        let axis = |code, max, res| UinputAbsSetup::new(code, AbsInfo::new(0, 0, max, 0, 0, res));
        let device = VirtualDevice::builder()?
            .name(DEVICE_NAME)
            .input_id(InputId::new(BusType::BUS_VIRTUAL, 0, 0, 1))
            .with_keys(&keys)?
            .with_properties(&props)?
            .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_X, XY_MAX, XY_RES))?
            .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_Y, XY_MAX, XY_RES))?
            .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_PRESSURE, PRESSURE_MAX, 0))?
            .build()?;
        Ok(Self {
            device,
            touching: false,
        })
    }

    fn emit(&mut self, frames: &[Frame]) -> io::Result<()> {
        for f in frames {
            let events: Vec<InputEvent> = f
                .iter()
                .map(|&(t, c, v)| InputEvent::new(t, c, v))
                .collect();
            // `emit` ends each batch with SYN_REPORT.
            self.device.emit(&events)?;
        }
        Ok(())
    }
}

impl PenDevice for UinputPen {
    fn send(
        &mut self,
        phase: PointerPhase,
        x: u16,
        y: u16,
        pressure: Option<u16>,
    ) -> io::Result<()> {
        let (out, touching) = frames(self.touching, phase, x, y, pressure);
        self.emit(&out)?;
        self.touching = touching;
        Ok(())
    }
}

impl Drop for UinputPen {
    fn drop(&mut self) {
        // Lift a pen that is still down so no app sees a stuck stroke.
        if self.touching {
            let _ = self.emit(&lift(None));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOUCH: u16 = KeyCode::BTN_TOUCH.0;
    const TOOL: u16 = KeyCode::BTN_TOOL_PEN.0;
    const X: u16 = AbsoluteAxisCode::ABS_X.0;
    const Y: u16 = AbsoluteAxisCode::ABS_Y.0;
    const P: u16 = AbsoluteAxisCode::ABS_PRESSURE.0;

    #[test]
    fn kernel_codes() {
        // linux/input-event-codes.h values the frames rely on.
        assert_eq!((KEY, ABS), (0x01, 0x03));
        assert_eq!((TOOL, TOUCH), (0x140, 0x14a));
        assert_eq!((X, Y, P), (0x00, 0x01, 0x18));
    }

    #[test]
    fn pressure_scale() {
        assert_eq!(device_pressure(None), PRESSURE_MAX);
        assert_eq!(device_pressure(Some(65_535)), PRESSURE_MAX);
        assert_eq!(device_pressure(Some(0)), 1);
        assert_eq!(device_pressure(Some(32_768)), 2047);
    }

    #[test]
    fn a_stroke() {
        let (down, t) = frames(false, PointerPhase::Down, 10, 20, Some(65_535));
        assert!(t);
        assert_eq!(
            down,
            vec![
                vec![(KEY, TOOL, 1), (ABS, X, 10), (ABS, Y, 20), (ABS, P, 0)],
                vec![(KEY, TOUCH, 1), (ABS, X, 10), (ABS, Y, 20), (ABS, P, 4095)],
            ]
        );
        let (mv, t) = frames(t, PointerPhase::Move, 30, 40, Some(0));
        assert!(t);
        assert_eq!(mv, vec![vec![(ABS, X, 30), (ABS, Y, 40), (ABS, P, 1)]]);
        let (up, t) = frames(t, PointerPhase::Up, 30, 40, None);
        assert!(!t);
        assert_eq!(
            up,
            vec![
                vec![(KEY, TOUCH, 0), (ABS, X, 30), (ABS, Y, 40), (ABS, P, 0)],
                vec![(KEY, TOOL, 0)],
            ]
        );
    }

    #[test]
    fn stray_events_do_nothing() {
        assert_eq!(
            frames(false, PointerPhase::Move, 1, 1, None),
            (Vec::new(), false)
        );
        assert_eq!(
            frames(false, PointerPhase::Up, 1, 1, None),
            (Vec::new(), false)
        );
        // A second down while touching only updates the touch frame.
        let (again, t) = frames(true, PointerPhase::Down, 5, 6, None);
        assert!(t);
        assert_eq!(again.len(), 1);
    }
}
