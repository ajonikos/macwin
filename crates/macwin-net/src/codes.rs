//! QUIC application close codes.

use macwin_core::engine::CloseReason;

/// Orderly close (session ended or shut down).
pub const NORMAL: u32 = 0;
/// The peer sent something invalid.
pub const PROTOCOL_VIOLATION: u32 = 1;
/// Protocol major versions differ.
pub const VERSION_MISMATCH: u32 = 2;
/// The peer's Engine stopped sending heartbeats.
pub const HEARTBEAT_TIMEOUT: u32 = 3;
/// The peer did not send its Hello in time.
pub const HELLO_TIMEOUT: u32 = 4;
/// Local failure (for example a send queue overflow).
pub const INTERNAL: u32 = 5;
/// Pairing failed: wrong code (or a device that does not know it).
pub const PAIRING_FAILED: u32 = 6;

/// The close code for an Engine close reason.
pub fn code_for(reason: &CloseReason) -> u32 {
    match reason {
        CloseReason::HeartbeatTimeout => HEARTBEAT_TIMEOUT,
        CloseReason::ProtocolViolation(_) => PROTOCOL_VIOLATION,
        CloseReason::Shutdown => NORMAL,
        CloseReason::Internal(_) => INTERNAL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_for_reasons() {
        assert_eq!(code_for(&CloseReason::Shutdown), NORMAL);
        assert_eq!(code_for(&CloseReason::HeartbeatTimeout), HEARTBEAT_TIMEOUT);
        assert_eq!(
            code_for(&CloseReason::ProtocolViolation("x")),
            PROTOCOL_VIOLATION
        );
        assert_eq!(code_for(&CloseReason::Internal("x")), INTERNAL);
    }
}
