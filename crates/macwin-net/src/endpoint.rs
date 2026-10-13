//! Listening and dialing.

use std::net::SocketAddr;

use macwin_core::msg::Hello;
use quinn::{Endpoint, VarInt};

use crate::fingerprint::Fingerprint;
use crate::identity::{CERT_NAME, Identity};
use crate::session::{self, Session, Side};
use crate::{NetError, codes, tls};

/// A bound listener that accepts only the pinned peer.
pub struct Listener {
    endpoint: Endpoint,
}

/// Binds a QUIC listener on `bind` presenting `id` and accepting only the
/// peer with fingerprint `peer`.
pub async fn listen(
    bind: SocketAddr,
    id: &Identity,
    peer: Fingerprint,
) -> Result<Listener, NetError> {
    let endpoint = Endpoint::server(tls::server_config(id, peer)?, bind)?;
    Ok(Listener { endpoint })
}

impl Listener {
    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        Ok(self.endpoint.local_addr()?)
    }

    /// Waits for the next incoming connection and returns its established
    /// session, or the error that ended its handshake. Callers decide whether
    /// to keep accepting after an error.
    pub async fn accept(&self, hello: &Hello) -> Result<Session, NetError> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or(NetError::EndpointClosed)?;
        let conn = incoming.await?;
        session::establish(self.endpoint.clone(), conn, Side::Listener, hello).await
    }

    /// Stops accepting and closes every connection of this listener.
    pub fn close(&self) {
        self.endpoint.close(VarInt::from_u32(codes::NORMAL), b"");
    }
}

/// Dials `remote` from a socket bound to `bind`, presenting `id` and
/// accepting only the peer with fingerprint `peer`.
pub async fn connect(
    bind: SocketAddr,
    remote: SocketAddr,
    id: &Identity,
    peer: Fingerprint,
    hello: &Hello,
) -> Result<Session, NetError> {
    let config = tls::client_config(id, peer)?;
    let endpoint = Endpoint::client(bind)?;
    let conn = endpoint.connect_with(config, remote, CERT_NAME)?.await?;
    session::establish(endpoint, conn, Side::Dialer, hello).await
}
