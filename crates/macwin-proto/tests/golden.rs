//! Golden bytes: these lock the wire format. If one of them changes, peers of
//! different versions stop understanding each other.

use macwin_core::msg::{ControlMsg, DatagramMsg, Heartbeat, InputMsg, KeyMsg, MouseAbs};
use macwin_core::types::{Hid, Point, RoleKind};
use macwin_proto::{
    decode_control, decode_datagram, decode_input, encode_control, encode_datagram, encode_input,
    frame,
};

#[test]
fn key_a_down() {
    let m = InputMsg::Key(KeyMsg {
        hid: Hid(0x07_0004),
        down: true,
        repeat: false,
    });
    let bytes = encode_input(&m);
    assert_eq!(bytes, [0x22, 0x06, 0x08, 0x84, 0x80, 0x1c, 0x10, 0x01]);
    assert_eq!(decode_input(&bytes).unwrap(), m);
    assert_eq!(
        &frame::encode(&bytes)[..],
        [0x08, 0x22, 0x06, 0x08, 0x84, 0x80, 0x1c, 0x10, 0x01]
    );
}

#[test]
fn heartbeat() {
    let m = ControlMsg::Heartbeat(Heartbeat {
        epoch: 3,
        role: RoleKind::Controlling,
    });
    let bytes = encode_control(&m);
    assert_eq!(bytes, [0x12, 0x04, 0x08, 0x03, 0x10, 0x02]);
    assert_eq!(decode_control(&bytes).unwrap(), m);
}

#[test]
fn mouse_abs_datagram() {
    let m = DatagramMsg::MouseAbs(MouseAbs {
        epoch: 1,
        seq: 2,
        at: Point::new(-1, 5),
    });
    let bytes = encode_datagram(&m);
    assert_eq!(
        bytes,
        [
            0x0A, 0x0A, 0x08, 0x01, 0x10, 0x02, 0x1A, 0x04, 0x08, 0x01, 0x10, 0x0A
        ]
    );
    assert_eq!(decode_datagram(&bytes).unwrap(), m);
}
