//! The macwin wire protocol: a strict protobuf codec for the domain messages
//! in `macwin_core::msg`, stream framing and version negotiation.
//!
//! Three frame families exist, one per channel: control stream, input stream
//! and datagrams. Decoding validates everything; invalid input is an error,
//! never a panic.
#![forbid(unsafe_code)]

mod convert;
pub mod frame;
pub mod wire;

use macwin_core::msg::{ClipboardMsg, ControlMsg, DatagramMsg, InputMsg, PairMsg};
use prost::Message;

/// `major << 16 | minor`. Peers with a different major refuse each other.
/// Minor 1 added clipboard streams (capability `clipboard.text`) and pairing.
pub const PROTOCOL_VERSION: u32 = 0x0001_0001;
/// Largest frame body on the control and input streams, the pairing stream
/// and in datagrams.
pub const MAX_FRAME_LEN: usize = 64 * 1024;
/// Largest clipboard text accepted on the wire, in UTF-8 bytes.
pub const MAX_CLIPBOARD_TEXT: usize = 16 * 1024 * 1024;
/// Largest encoded [`ClipboardMsg`] (the text plus protobuf overhead).
pub const MAX_CLIPBOARD_FRAME: usize = MAX_CLIPBOARD_TEXT + 64;
/// Length of a SPAKE2 (Ed25519) message.
pub const SPAKE_MSG_LEN: usize = 33;
/// Length of a pairing key-confirmation MAC (HMAC-SHA256).
pub const PAIR_MAC_LEN: usize = 32;
/// Largest accepted string field, in bytes.
pub const MAX_STRING_LEN: usize = 256;
/// Most capabilities accepted in a Hello.
pub const MAX_CAPABILITIES: usize = 32;
/// Longest accepted capability, in bytes.
pub const MAX_CAPABILITY_LEN: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("malformed protobuf: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("missing field {0}")]
    MissingField(&'static str),
    #[error("missing or unknown message kind in {0}")]
    UnknownKind(&'static str),
    #[error("invalid value {value} for enum field {field}")]
    InvalidEnum { field: &'static str, value: i32 },
    #[error("invalid {field}: {reason}")]
    InvalidValue {
        field: &'static str,
        reason: &'static str,
    },
    #[error("frame of {len} bytes exceeds the 65536-byte limit")]
    FrameTooLarge { len: usize },
    #[error("incompatible protocol versions: local {local:#010x}, peer {peer:#010x}")]
    IncompatibleVersion { local: u32, peer: u32 },
}

/// The major part of a protocol version.
pub const fn version_major(v: u32) -> u16 {
    (v >> 16) as u16
}

/// Accepts a peer with the same major version as [`PROTOCOL_VERSION`].
pub fn check_version(peer: u32) -> Result<(), ProtoError> {
    check_versions(PROTOCOL_VERSION, peer)
}

/// Accepts `peer` when both versions are non-zero and share the major part.
/// Each side checks the version it announced itself against the peer's, so
/// a mismatch is detected on both sides.
pub fn check_versions(local: u32, peer: u32) -> Result<(), ProtoError> {
    if local == 0 || peer == 0 || version_major(local) != version_major(peer) {
        return Err(ProtoError::IncompatibleVersion { local, peer });
    }
    Ok(())
}

pub fn encode_control(m: &ControlMsg) -> Vec<u8> {
    convert::control_to_wire(m).encode_to_vec()
}

pub fn decode_control(b: &[u8]) -> Result<ControlMsg, ProtoError> {
    convert::control_from_wire(wire::ControlFrame::decode(b)?)
}

pub fn encode_input(m: &InputMsg) -> Vec<u8> {
    convert::input_to_wire(m).encode_to_vec()
}

pub fn decode_input(b: &[u8]) -> Result<InputMsg, ProtoError> {
    convert::input_from_wire(wire::InputFrame::decode(b)?)
}

pub fn encode_datagram(m: &DatagramMsg) -> Vec<u8> {
    convert::datagram_to_wire(m).encode_to_vec()
}

pub fn decode_datagram(b: &[u8]) -> Result<DatagramMsg, ProtoError> {
    convert::datagram_from_wire(wire::DatagramFrame::decode(b)?)
}

pub fn encode_clipboard(m: &ClipboardMsg) -> Vec<u8> {
    convert::clipboard_to_wire(m).encode_to_vec()
}

/// True when the peer will accept `m` ([`decode_clipboard`]'s limits).
pub fn clipboard_within_limits(m: &ClipboardMsg) -> bool {
    match m {
        ClipboardMsg::Text(t) => t.text.len() <= MAX_CLIPBOARD_TEXT,
        ClipboardMsg::Other { .. } => true,
    }
}

pub fn decode_clipboard(b: &[u8]) -> Result<ClipboardMsg, ProtoError> {
    if b.len() > MAX_CLIPBOARD_FRAME {
        return Err(ProtoError::InvalidValue {
            field: "clipboard frame",
            reason: "frame larger than the clipboard limit",
        });
    }
    convert::clipboard_from_wire(wire::ClipboardFrame::decode(b)?)
}

pub fn encode_pair(m: &PairMsg) -> Vec<u8> {
    convert::pair_to_wire(m).encode_to_vec()
}

pub fn decode_pair(b: &[u8]) -> Result<PairMsg, ProtoError> {
    convert::pair_from_wire(wire::PairFrame::decode(b)?)
}

#[cfg(test)]
mod tests;
