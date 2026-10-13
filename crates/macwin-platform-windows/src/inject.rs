//! Injection with SendInput. Every input carries `INJECT_TAG` in
//! `dwExtraInfo`; our hooks skip injected input anyway (`LL*HF_INJECTED`).

use macwin_core::os::INJECT_TAG;
use macwin_core::os::windows::{
    WinKey, XBUTTON1, XBUTTON2, key_injection, normalize_absolute, wheel_data,
};
use macwin_core::types::{Hid, InjectCommand, MouseButton, Point};
use macwin_platform::{InputInjector, PlatformError};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
    SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

/// SendInput injection backend.
#[derive(Debug, Default)]
pub struct WinInjector;

impl WinInjector {
    pub fn new() -> WinInjector {
        WinInjector
    }
}

fn send(input: INPUT) -> Result<(), PlatformError> {
    // SAFETY: one fully initialised INPUT and its exact size.
    let sent = unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
    if sent == 1 {
        Ok(())
    } else {
        Err(PlatformError::Backend(
            "SendInput was blocked: an elevated window has focus (UIPI) or the secure desktop \
             (lock screen / UAC) is active"
                .into(),
        ))
    }
}

fn mouse(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECT_TAG as usize,
            },
        },
    }
}

fn key(hid: Hid, down: bool) -> Option<INPUT> {
    let up = if down { 0 } else { KEYEVENTF_KEYUP };
    let ext = |e: bool| if e { KEYEVENTF_EXTENDEDKEY } else { 0 };
    let ki = match key_injection(hid)? {
        WinKey::Scancode { sc, extended } => KEYBDINPUT {
            wVk: 0,
            wScan: sc,
            dwFlags: KEYEVENTF_SCANCODE | ext(extended) | up,
            time: 0,
            dwExtraInfo: INJECT_TAG as usize,
        },
        WinKey::Vk { vk, extended } => KEYBDINPUT {
            wVk: vk,
            wScan: 0,
            dwFlags: ext(extended) | up,
            time: 0,
            dwExtraInfo: INJECT_TAG as usize,
        },
    };
    Some(INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki },
    })
}

/// `SendInput` absolute coordinates for a virtual-screen pixel.
fn absolute(p: Point) -> (i32, i32) {
    // SAFETY: plain metric queries.
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    (
        normalize_absolute(p.x, vx, vw),
        normalize_absolute(p.y, vy, vh),
    )
}

impl InputInjector for WinInjector {
    fn inject(&mut self, cmd: InjectCommand) -> Result<(), PlatformError> {
        match cmd {
            InjectCommand::MoveTo(p) => {
                let (x, y) = absolute(p);
                send(mouse(
                    x,
                    y,
                    0,
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                ))
            }
            InjectCommand::Button { button, down, .. } => {
                let (flags, data) = match (button, down) {
                    (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                    (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
                    (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                    (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
                    (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                    (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                    (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
                    (MouseButton::Back, false) => (MOUSEEVENTF_XUP, XBUTTON1),
                    (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
                    (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, XBUTTON2),
                };
                send(mouse(0, 0, data, flags))
            }
            InjectCommand::Wheel { dx, dy, unit } => {
                let (dx, dy) = (wheel_data(dx, unit), wheel_data(dy, unit));
                if dy != 0 {
                    send(mouse(0, 0, dy as u32, MOUSEEVENTF_WHEEL))?;
                }
                if dx != 0 {
                    send(mouse(0, 0, dx as u32, MOUSEEVENTF_HWHEEL))?;
                }
                Ok(())
            }
            InjectCommand::Key { hid, down, .. } => match key(hid, down) {
                Some(input) => send(input),
                None => Ok(()), // no Windows key for this usage
            },
        }
    }
}
