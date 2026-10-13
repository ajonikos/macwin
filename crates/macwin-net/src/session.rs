//! Session establishment on top of an authenticated QUIC connection.
//!
//! The dialer opens two bidirectional streams and writes a one-byte header
//! on each (0x01 control, 0x02 input) so the listener's `accept_bi` returns
//! and can tell them apart. Both sides then send their Hello on the control
//! stream and read the peer's within [`HELLO_TIMEOUT`].

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use macwin_core::msg::{ControlMsg, Hello};
use macwin_proto::frame::{self, FrameBuffer};
use macwin_proto::{ProtoError, check_versions, decode_control, encode_control};
use quinn::{
    Connection, ConnectionError, Endpoint, ReadError, ReadExactError, RecvStream, SendStream,
    VarInt, WriteError,
};
use tracing::{debug, info};

use crate::link::{self, Link, Streams};
use crate::{NetError, codes};

/// How long each side waits for the peer's Hello (and, on the listener, for
/// the dialer's streams).
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

const HEADER_CONTROL: u8 = 0x01;
const HEADER_INPUT: u8 = 0x02;
/// Priority of the input stream's sends (control stays at 0).
const INPUT_PRIORITY: i32 = 1;

/// An established session.
pub struct Session {
    /// The peer's Hello (validated, version-compatible).
    pub peer: Hello,
    pub link: Link,
    pub remote: SocketAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    Dialer,
    Listener,
}

fn write_err(e: WriteError) -> NetError {
    match e {
        WriteError::ConnectionLost(c) => NetError::Connection(c),
        _ => NetError::StreamSetup("stream write failed"),
    }
}

fn read_exact_err(e: ReadExactError) -> NetError {
    match e {
        ReadExactError::ReadError(ReadError::ConnectionLost(c)) => NetError::Connection(c),
        _ => NetError::StreamSetup("stream ended during setup"),
    }
}

fn close(conn: &Connection, code: u32, why: &'static [u8]) {
    conn.close(VarInt::from_u32(code), why);
}

async fn open_streams(
    conn: &Connection,
) -> Result<(SendStream, RecvStream, SendStream, RecvStream), NetError> {
    let (mut cs, cr) = conn.open_bi().await?;
    cs.write_all(&[HEADER_CONTROL]).await.map_err(write_err)?;
    let (mut is, ir) = conn.open_bi().await?;
    is.write_all(&[HEADER_INPUT]).await.map_err(write_err)?;
    Ok((cs, cr, is, ir))
}

async fn accept_streams(
    conn: &Connection,
) -> Result<(SendStream, RecvStream, SendStream, RecvStream), NetError> {
    let mut control = None;
    let mut input = None;
    for _ in 0..2 {
        let (s, mut r) = conn.accept_bi().await?;
        let mut header = [0u8; 1];
        r.read_exact(&mut header).await.map_err(read_exact_err)?;
        match header[0] {
            HEADER_CONTROL if control.is_none() => control = Some((s, r)),
            HEADER_INPUT if input.is_none() => input = Some((s, r)),
            _ => {
                close(conn, codes::PROTOCOL_VIOLATION, b"bad stream header");
                return Err(NetError::StreamSetup("unexpected stream header"));
            }
        }
    }
    match (control, input) {
        (Some((cs, cr)), Some((is, ir))) => Ok((cs, cr, is, ir)),
        _ => Err(NetError::StreamSetup("missing stream")),
    }
}

async fn read_first_frame(recv: &mut RecvStream, buf: &mut FrameBuffer) -> Result<Bytes, NetError> {
    loop {
        if let Some(body) = buf.next_frame()? {
            return Ok(body);
        }
        match recv.read_chunk(64 * 1024, true).await {
            Ok(Some(chunk)) => buf.push(&chunk.bytes),
            Ok(None) => {
                return Err(NetError::StreamSetup(
                    "control stream ended before the hello",
                ));
            }
            Err(ReadError::ConnectionLost(c)) => return Err(NetError::Connection(c)),
            Err(_) => {
                return Err(NetError::StreamSetup(
                    "control stream failed before the hello",
                ));
            }
        }
    }
}

/// Opens or accepts the streams, exchanges Hellos and starts the link.
pub(crate) async fn establish(
    endpoint: Endpoint,
    conn: Connection,
    side: Side,
    hello: &Hello,
) -> Result<Session, NetError> {
    let streams = match side {
        Side::Dialer => open_streams(&conn).await,
        Side::Listener => match tokio::time::timeout(HELLO_TIMEOUT, accept_streams(&conn)).await {
            Ok(r) => r,
            Err(_) => {
                close(&conn, codes::HELLO_TIMEOUT, b"no streams");
                return Err(NetError::HelloTimeout);
            }
        },
    };
    let (mut control_send, mut control_recv, input_send, input_recv) = streams?;
    let _ = input_send.set_priority(INPUT_PRIORITY);

    let ours = frame::encode(&encode_control(&ControlMsg::Hello(hello.clone())));
    control_send.write_all(&ours).await.map_err(write_err)?;

    let mut control_buf = FrameBuffer::new();
    let first = match tokio::time::timeout(
        HELLO_TIMEOUT,
        read_first_frame(&mut control_recv, &mut control_buf),
    )
    .await
    {
        Ok(Ok(body)) => body,
        // The peer refused our version before its own Hello left its send
        // buffer: report the same error it saw.
        Ok(Err(NetError::Connection(ConnectionError::ApplicationClosed(c))))
            if c.error_code == VarInt::from_u32(codes::VERSION_MISMATCH) =>
        {
            return Err(NetError::Proto(ProtoError::IncompatibleVersion {
                local: hello.protocol_version,
                peer: 0,
            }));
        }
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            close(&conn, codes::HELLO_TIMEOUT, b"no hello");
            return Err(NetError::HelloTimeout);
        }
    };
    let peer = match decode_control(&first) {
        Ok(ControlMsg::Hello(h)) => h,
        Ok(other) => {
            debug!(?other, "first control frame is not a hello");
            close(&conn, codes::PROTOCOL_VIOLATION, b"expected hello");
            return Err(NetError::StreamSetup(
                "the first control frame is not a hello",
            ));
        }
        Err(e) => {
            close(&conn, codes::PROTOCOL_VIOLATION, b"bad hello");
            return Err(NetError::Proto(e));
        }
    };
    if let Err(e) = check_versions(hello.protocol_version, peer.protocol_version) {
        close(&conn, codes::VERSION_MISMATCH, b"version mismatch");
        return Err(NetError::Proto(e));
    }
    let remote = conn.remote_address();
    info!(%remote, peer = %peer.device_name, ?side, "session established");
    let link = link::spawn_quic(
        endpoint,
        conn,
        Streams {
            control_send,
            control_recv,
            control_buf,
            input_send,
            input_recv,
        },
    );
    Ok(Session { peer, link, remote })
}
