//! A transport-agnostic message link: what the driver talks to.
//!
//! The QUIC implementation runs one writer task per stream, one reader task
//! per stream, one datagram reader and a supervisor. Every decode error on
//! any channel closes the connection as a protocol violation. Exactly one
//! [`Inbound::Closed`] is delivered per link, after every message that
//! arrived before the close.
//!
//! [`Link::memory_pair`] is an in-process implementation for tests.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bytes::Bytes;
use macwin_core::msg::{ControlMsg, DatagramMsg, InputMsg};
use macwin_proto::frame::{self, FrameBuffer};
use macwin_proto::{
    ProtoError, decode_control, decode_datagram, decode_input, encode_control, encode_datagram,
    encode_input,
};
use quinn::{Connection, ConnectionError, Endpoint, RecvStream, SendStream, VarInt};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::codes;

/// Outgoing queue length per stream; overflowing it closes the link.
pub const QUEUE_CAPACITY: usize = 4096;
/// Inbound queue length.
pub const INBOUND_CAPACITY: usize = 4096;
/// Bound on flushing and finishing streams during a graceful close.
pub const GRACEFUL_CLOSE_TIMEOUT: Duration = Duration::from_millis(500);
/// Bound on waiting for the close to reach the peer in [`LinkSender::wait_closed`].
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const READ_CHUNK: usize = 64 * 1024;

/// Something that arrived on the link.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    Control(ControlMsg),
    Input(InputMsg),
    Datagram(DatagramMsg),
    /// The link is gone; always the last item.
    Closed(LinkClosed),
}

/// Why a link closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkClosed {
    /// The peer closed with an application close code.
    PeerClosed { code: u64 },
    /// This side closed with the given code.
    LocalClosed { code: u64 },
    /// No packets from the peer within the idle timeout.
    TimedOut,
    /// Any other transport-level failure.
    Transport(String),
    /// The peer sent something undecodable; this side closed the link.
    ProtocolViolation(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error("the link is closed")]
    Closed,
    #[error("send queue overflow; the link was closed")]
    Overflow,
}

/// Both halves of a link.
pub struct Link {
    pub tx: LinkSender,
    pub rx: mpsc::Receiver<Inbound>,
}

/// Cheap-to-clone sending half. All methods are non-blocking and safe to
/// call from any thread inside a tokio runtime context or not.
#[derive(Clone)]
pub struct LinkSender {
    inner: Arc<Inner>,
}

enum Inner {
    Quic(QuicShared),
    Memory(MemoryShared),
}

type Reason = Arc<Mutex<Option<LinkClosed>>>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Records why the link closed; the first reason wins.
fn set_reason(reason: &Mutex<Option<LinkClosed>>, r: LinkClosed) {
    let mut slot = lock(reason);
    if slot.is_none() {
        *slot = Some(r);
    }
}

impl LinkSender {
    pub fn send_control(&self, m: ControlMsg) -> Result<(), LinkError> {
        match &*self.inner {
            Inner::Quic(q) => q.send_framed(&q.control, &encode_control(&m)),
            Inner::Memory(mem) => mem.send(Inbound::Control(m)),
        }
    }

    pub fn send_input(&self, m: InputMsg) -> Result<(), LinkError> {
        match &*self.inner {
            Inner::Quic(q) => q.send_framed(&q.input, &encode_input(&m)),
            Inner::Memory(mem) => mem.send(Inbound::Input(m)),
        }
    }

    /// Unreliable: dropped silently when the link is closed or congested.
    pub fn send_datagram(&self, m: DatagramMsg) {
        match &*self.inner {
            Inner::Quic(q) => {
                if q.closing.load(Ordering::Acquire) {
                    return;
                }
                if let Err(e) = q.conn.send_datagram(Bytes::from(encode_datagram(&m))) {
                    debug!(error = %e, "datagram dropped");
                }
            }
            Inner::Memory(mem) => {
                if !mem.closed.load(Ordering::Acquire) {
                    let _ = mem.peer.try_send(Inbound::Datagram(m));
                }
            }
        }
    }

    /// Graceful close: flushes and finishes both streams (bounded by
    /// [`GRACEFUL_CLOSE_TIMEOUT`]), then closes the connection with `code`.
    /// Later sends fail with [`LinkError::Closed`].
    pub fn close(&self, code: u32) {
        match &*self.inner {
            Inner::Quic(q) => q.close(code),
            Inner::Memory(mem) => mem.close(code),
        }
    }

    /// Immediate close; queued messages may be lost.
    pub fn close_now(&self, code: u32) {
        match &*self.inner {
            Inner::Quic(q) => q.close_now(code),
            Inner::Memory(mem) => mem.close(code),
        }
    }

    pub fn is_closed(&self) -> bool {
        match &*self.inner {
            Inner::Quic(q) => q.closing.load(Ordering::Acquire) || q.conn.close_reason().is_some(),
            Inner::Memory(mem) => mem.closed.load(Ordering::Acquire),
        }
    }

    /// Waits until the connection is closed and the close has been sent to
    /// the peer (bounded by [`DRAIN_TIMEOUT`]). Call before exiting a
    /// process so the peer sees an orderly close instead of a timeout.
    pub async fn wait_closed(&self) {
        if let Inner::Quic(q) = &*self.inner {
            q.conn.closed().await;
            let _ = tokio::time::timeout(DRAIN_TIMEOUT, q.endpoint.wait_idle()).await;
        }
    }
}

struct QuicShared {
    conn: Connection,
    /// Kept so the endpoint outlives every user of the connection.
    endpoint: Endpoint,
    control: mpsc::Sender<Bytes>,
    input: mpsc::Sender<Bytes>,
    closing: AtomicBool,
    finish: watch::Sender<bool>,
    writers: Mutex<Vec<JoinHandle<()>>>,
    reason: Reason,
    runtime: tokio::runtime::Handle,
}

impl QuicShared {
    fn send_framed(&self, queue: &mpsc::Sender<Bytes>, body: &[u8]) -> Result<(), LinkError> {
        if self.closing.load(Ordering::Acquire) {
            return Err(LinkError::Closed);
        }
        match queue.try_send(frame::encode(body)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                warn!("send queue overflow; closing the link");
                self.close_now(codes::INTERNAL);
                Err(LinkError::Overflow)
            }
            Err(TrySendError::Closed(_)) => Err(LinkError::Closed),
        }
    }

    fn close(&self, code: u32) {
        if self.closing.swap(true, Ordering::AcqRel) {
            return;
        }
        set_reason(
            &self.reason,
            LinkClosed::LocalClosed {
                code: u64::from(code),
            },
        );
        let _ = self.finish.send(true);
        let writers = std::mem::take(&mut *lock(&self.writers));
        let conn = self.conn.clone();
        self.runtime.spawn(async move {
            let flush = async {
                for w in writers {
                    let _ = w.await;
                }
            };
            if tokio::time::timeout(GRACEFUL_CLOSE_TIMEOUT, flush)
                .await
                .is_err()
            {
                debug!("graceful close timed out; closing anyway");
            }
            conn.close(VarInt::from_u32(code), b"");
        });
    }

    fn close_now(&self, code: u32) {
        self.closing.store(true, Ordering::Release);
        set_reason(
            &self.reason,
            LinkClosed::LocalClosed {
                code: u64::from(code),
            },
        );
        self.conn.close(VarInt::from_u32(code), b"");
    }
}

impl Drop for QuicShared {
    fn drop(&mut self) {
        // Every sender is gone: nobody can use the connection any more.
        if !self.closing.load(Ordering::Acquire) {
            self.close_now(codes::NORMAL);
        }
    }
}

struct MemoryShared {
    peer: mpsc::Sender<Inbound>,
    own: mpsc::Sender<Inbound>,
    closed: Arc<AtomicBool>,
}

impl MemoryShared {
    fn send(&self, m: Inbound) -> Result<(), LinkError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(LinkError::Closed);
        }
        match self.peer.try_send(m) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.close(codes::INTERNAL);
                Err(LinkError::Overflow)
            }
            Err(TrySendError::Closed(_)) => Err(LinkError::Closed),
        }
    }

    fn close(&self, code: u32) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            let code = u64::from(code);
            let _ = self
                .peer
                .try_send(Inbound::Closed(LinkClosed::PeerClosed { code }));
            let _ = self
                .own
                .try_send(Inbound::Closed(LinkClosed::LocalClosed { code }));
        }
    }
}

impl Link {
    /// Two connected in-process links (no QUIC), for driver unit tests.
    pub fn memory_pair() -> (Link, Link) {
        // One extra slot so the final Closed always fits.
        let (a_tx, a_rx) = mpsc::channel(INBOUND_CAPACITY + 1);
        let (b_tx, b_rx) = mpsc::channel(INBOUND_CAPACITY + 1);
        let closed = Arc::new(AtomicBool::new(false));
        let a = Link {
            tx: LinkSender {
                inner: Arc::new(Inner::Memory(MemoryShared {
                    peer: b_tx.clone(),
                    own: a_tx.clone(),
                    closed: closed.clone(),
                })),
            },
            rx: a_rx,
        };
        let b = Link {
            tx: LinkSender {
                inner: Arc::new(Inner::Memory(MemoryShared {
                    peer: a_tx,
                    own: b_tx,
                    closed,
                })),
            },
            rx: b_rx,
        };
        (a, b)
    }
}

/// The streams of an established session, handed over by `session.rs`.
pub(crate) struct Streams {
    pub(crate) control_send: SendStream,
    pub(crate) control_recv: RecvStream,
    /// Bytes already read from the control stream past the Hello.
    pub(crate) control_buf: FrameBuffer,
    pub(crate) input_send: SendStream,
    pub(crate) input_recv: RecvStream,
}

/// Starts the QUIC tasks of an established session.
pub(crate) fn spawn_quic(endpoint: Endpoint, conn: Connection, streams: Streams) -> Link {
    let runtime = tokio::runtime::Handle::current();
    let (inbound_tx, inbound_rx) = mpsc::channel(INBOUND_CAPACITY);
    let (control_tx, control_rx) = mpsc::channel(QUEUE_CAPACITY);
    let (input_tx, input_rx) = mpsc::channel(QUEUE_CAPACITY);
    let (finish_tx, finish_rx) = watch::channel(false);
    let reason: Reason = Arc::new(Mutex::new(None));
    let writers = vec![
        runtime.spawn(writer(streams.control_send, control_rx, finish_rx.clone())),
        runtime.spawn(writer(streams.input_send, input_rx, finish_rx)),
    ];
    let readers = vec![
        runtime.spawn(stream_reader(
            streams.control_recv,
            streams.control_buf,
            Channel::Control,
            inbound_tx.clone(),
            conn.clone(),
            reason.clone(),
        )),
        runtime.spawn(stream_reader(
            streams.input_recv,
            FrameBuffer::new(),
            Channel::Input,
            inbound_tx.clone(),
            conn.clone(),
            reason.clone(),
        )),
        runtime.spawn(datagram_reader(
            conn.clone(),
            inbound_tx.clone(),
            reason.clone(),
        )),
    ];
    runtime.spawn(supervisor(
        conn.clone(),
        readers,
        inbound_tx,
        reason.clone(),
    ));
    Link {
        tx: LinkSender {
            inner: Arc::new(Inner::Quic(QuicShared {
                conn,
                endpoint,
                control: control_tx,
                input: input_tx,
                closing: AtomicBool::new(false),
                finish: finish_tx,
                writers: Mutex::new(writers),
                reason,
                runtime,
            })),
        },
        rx: inbound_rx,
    }
}

#[derive(Debug, Clone, Copy)]
enum Channel {
    Control,
    Input,
}

fn decode(ch: Channel, body: &[u8]) -> Result<Inbound, ProtoError> {
    match ch {
        Channel::Control => decode_control(body).map(Inbound::Control),
        Channel::Input => decode_input(body).map(Inbound::Input),
    }
}

fn violation(conn: &Connection, reason: &Mutex<Option<LinkClosed>>, msg: String) {
    warn!(%msg, "peer protocol violation; closing the link");
    set_reason(reason, LinkClosed::ProtocolViolation(msg));
    conn.close(
        VarInt::from_u32(codes::PROTOCOL_VIOLATION),
        b"protocol violation",
    );
}

async fn writer(
    mut send: SendStream,
    mut rx: mpsc::Receiver<Bytes>,
    mut finish: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            biased;
            msg = rx.recv() => match msg {
                Some(bytes) => {
                    if send.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
                None => break,
            },
            // The async block drops the watch guard (which is !Send) before
            // the branch completes.
            () = async { let _ = finish.wait_for(|f| *f).await; } => break,
        }
    }
    // Graceful close: flush what is queued, finish, wait for the peer's ACK.
    while let Ok(bytes) = rx.try_recv() {
        if send.write_all(&bytes).await.is_err() {
            return;
        }
    }
    if send.finish().is_ok() {
        let _ = send.stopped().await;
    }
}

async fn stream_reader(
    mut recv: RecvStream,
    mut buf: FrameBuffer,
    ch: Channel,
    tx: mpsc::Sender<Inbound>,
    conn: Connection,
    reason: Reason,
) {
    loop {
        loop {
            match buf.next_frame() {
                Ok(Some(body)) => match decode(ch, &body) {
                    Ok(msg) => {
                        if tx.send(msg).await.is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        violation(&conn, &reason, format!("{ch:?} frame: {e}"));
                        return;
                    }
                },
                Ok(None) => break,
                Err(e) => {
                    violation(&conn, &reason, format!("{ch:?} framing: {e}"));
                    return;
                }
            }
        }
        match recv.read_chunk(READ_CHUNK, true).await {
            Ok(Some(chunk)) => buf.push(&chunk.bytes),
            Ok(None) => {
                debug!(?ch, "peer finished its stream");
                return;
            }
            Err(e) => {
                debug!(?ch, error = %e, "stream read ended");
                return;
            }
        }
    }
}

async fn datagram_reader(conn: Connection, tx: mpsc::Sender<Inbound>, reason: Reason) {
    loop {
        let Ok(bytes) = conn.read_datagram().await else {
            return;
        };
        match decode_datagram(&bytes) {
            Ok(m) => {
                if tx.send(Inbound::Datagram(m)).await.is_err() {
                    return;
                }
            }
            Err(e) => {
                violation(&conn, &reason, format!("datagram: {e}"));
                return;
            }
        }
    }
}

fn map_close(e: &ConnectionError) -> LinkClosed {
    match e {
        ConnectionError::ApplicationClosed(c) => LinkClosed::PeerClosed {
            code: c.error_code.into_inner(),
        },
        ConnectionError::LocallyClosed => LinkClosed::LocalClosed {
            code: u64::from(codes::NORMAL),
        },
        ConnectionError::TimedOut => LinkClosed::TimedOut,
        other => LinkClosed::Transport(other.to_string()),
    }
}

/// Waits for the connection to close and for every reader to drain, then
/// reports exactly one `Closed`.
async fn supervisor(
    conn: Connection,
    readers: Vec<JoinHandle<()>>,
    tx: mpsc::Sender<Inbound>,
    reason: Reason,
) {
    let err = conn.closed().await;
    for r in readers {
        let _ = r.await;
    }
    let closed = lock(&reason).take().unwrap_or_else(|| map_close(&err));
    debug!(?closed, "link closed");
    let _ = tx.send(Inbound::Closed(closed)).await;
}

#[cfg(test)]
mod tests {
    use macwin_core::msg::{Leave, Ping};

    use super::*;

    #[tokio::test]
    async fn memory_pair_round_trip_and_single_close() {
        let (a, mut b) = Link::memory_pair();
        let ping = Ping {
            id: 1,
            sent_micros: 2,
        };
        a.tx.send_control(ControlMsg::Ping(ping)).unwrap();
        a.tx.send_input(InputMsg::Leave(Leave { epoch: 3 }))
            .unwrap();
        a.tx.send_datagram(DatagramMsg::Ping(ping));
        assert_eq!(
            b.rx.recv().await,
            Some(Inbound::Control(ControlMsg::Ping(ping)))
        );
        assert_eq!(
            b.rx.recv().await,
            Some(Inbound::Input(InputMsg::Leave(Leave { epoch: 3 })))
        );
        assert_eq!(
            b.rx.recv().await,
            Some(Inbound::Datagram(DatagramMsg::Ping(ping)))
        );
        b.tx.close(codes::INTERNAL);
        b.tx.close(codes::NORMAL); // second close is a no-op
        let mut a = a;
        assert_eq!(
            a.rx.recv().await,
            Some(Inbound::Closed(LinkClosed::PeerClosed { code: 5 }))
        );
        assert_eq!(
            b.rx.recv().await,
            Some(Inbound::Closed(LinkClosed::LocalClosed { code: 5 }))
        );
        assert_eq!(
            a.tx.send_control(ControlMsg::Ping(ping)),
            Err(LinkError::Closed)
        );
        assert!(a.tx.is_closed() && b.tx.is_closed());
    }
}
