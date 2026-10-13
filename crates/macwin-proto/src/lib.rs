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

use macwin_core::msg::{ControlMsg, DatagramMsg, InputMsg};
use prost::Message;

/// `major << 16 | minor`. Peers with a different major refuse each other.
pub const PROTOCOL_VERSION: u32 = 0x0001_0000;
/// Largest frame body, for streams and datagrams.
pub const MAX_FRAME_LEN: usize = 64 * 1024;
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

#[cfg(test)]
mod tests;
