//! TLS 1.3 configuration with mutual certificate pinning.
//!
//! Both sides present their self-signed certificate and accept exactly one
//! peer certificate, identified by its SHA-256 fingerprint. Sessions (ALPN
//! `macwin/1`) are only ever built with these pinning verifiers: macwin has
//! no unauthenticated or unencrypted mode.
//!
//! The one exception is pairing (ALPN `macwin-pair/1`, crate-private
//! configs): it accepts any certificate because the SPAKE2 exchange that
//! runs inside the connection authenticates both certificates with the
//! pairing code (see `pairing`). A pairing connection never carries session
//! traffic, and the different ALPN keeps the two from ever being mixed up.

use std::sync::Arc;
use std::time::Duration;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{CertificateError, DigitallySignedStruct, DistinguishedName, SignatureScheme};
use tracing::warn;

use crate::NetError;
use crate::fingerprint::Fingerprint;
use crate::identity::Identity;

/// ALPN protocol id of sessions.
pub const ALPN: &[u8] = b"macwin/1";
/// ALPN protocol id of the pairing exchange.
pub const PAIR_ALPN: &[u8] = b"macwin-pair/1";
/// Unidirectional streams the peer may have open at once in a session
/// (clipboard transfers).
pub const MAX_UNI_STREAMS: u32 = 16;
/// QUIC idle timeout.
pub const MAX_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
/// QUIC keep-alive interval.
pub const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(1);

/// The ring provider, always passed explicitly (never installed globally).
fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn check_pin(
    expected: Fingerprint,
    cert: &CertificateDer<'_>,
    who: &str,
) -> Result<(), rustls::Error> {
    let actual = Fingerprint::of_cert(cert.as_ref());
    if actual == expected {
        return Ok(());
    }
    warn!(%expected, %actual, "{who} certificate does not match the pinned fingerprint");
    Err(rustls::Error::InvalidCertificate(
        CertificateError::ApplicationVerificationFailure,
    ))
}

fn no_tls12() -> rustls::Error {
    rustls::Error::General("TLS 1.2 is not supported".into())
}

/// Client-side verifier: accepts only the pinned server certificate. The
/// server name and intermediates are ignored.
#[derive(Debug)]
pub struct PinnedServerVerifier {
    expected: Fingerprint,
    provider: Arc<CryptoProvider>,
}

impl PinnedServerVerifier {
    pub fn new(expected: Fingerprint) -> PinnedServerVerifier {
        PinnedServerVerifier {
            expected,
            provider: provider(),
        }
    }
}

impl ServerCertVerifier for PinnedServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        check_pin(self.expected, end_entity, "server")?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(no_tls12())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Server-side verifier: requires a client certificate and accepts only the
/// pinned one.
#[derive(Debug)]
pub struct PinnedClientVerifier {
    expected: Fingerprint,
    provider: Arc<CryptoProvider>,
}

impl PinnedClientVerifier {
    pub fn new(expected: Fingerprint) -> PinnedClientVerifier {
        PinnedClientVerifier {
            expected,
            provider: provider(),
        }
    }
}

impl ClientCertVerifier for PinnedClientVerifier {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        check_pin(self.expected, end_entity, "client")?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(no_tls12())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Pairing only: accepts any server certificate, still verifying that the
/// server holds its key. The SPAKE2 exchange authenticates the certificate.
#[derive(Debug)]
struct AnyServerCert {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for AnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(no_tls12())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Pairing only: requires a client certificate and accepts any, still
/// verifying that the client holds its key.
#[derive(Debug)]
struct AnyClientCert {
    provider: Arc<CryptoProvider>,
}

impl ClientCertVerifier for AnyClientCert {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(no_tls12())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// `bidi` / `uni`: streams of each kind the peer may have open at once.
fn transport(bidi: u32, uni: u32) -> Arc<quinn::TransportConfig> {
    let mut t = quinn::TransportConfig::default();
    t.max_idle_timeout(MAX_IDLE_TIMEOUT.try_into().ok());
    t.keep_alive_interval(Some(KEEP_ALIVE_INTERVAL));
    t.max_concurrent_bidi_streams(bidi.into());
    t.max_concurrent_uni_streams(uni.into());
    Arc::new(t)
}

/// Session streams: the dialer opens the control and the input stream.
fn session_transport() -> Arc<quinn::TransportConfig> {
    transport(2, MAX_UNI_STREAMS)
}

/// Pairing: one bidirectional stream opened by the dialer.
fn pairing_transport() -> Arc<quinn::TransportConfig> {
    transport(1, 0)
}

fn build_client(
    id: &Identity,
    verifier: Arc<dyn ServerCertVerifier>,
    alpn: &[u8],
    transport: Arc<quinn::TransportConfig>,
) -> Result<quinn::ClientConfig, NetError> {
    let mut tls = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![id.cert().clone()], id.key())?;
    tls.alpn_protocols = vec![alpn.to_vec()];
    let quic = QuicClientConfig::try_from(tls).map_err(|e| NetError::QuicConfig(e.to_string()))?;
    let mut cfg = quinn::ClientConfig::new(Arc::new(quic));
    cfg.transport_config(transport);
    Ok(cfg)
}

fn build_server(
    id: &Identity,
    verifier: Arc<dyn ClientCertVerifier>,
    alpn: &[u8],
    transport: Arc<quinn::TransportConfig>,
) -> Result<quinn::ServerConfig, NetError> {
    let mut tls = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![id.cert().clone()], id.key())?;
    tls.alpn_protocols = vec![alpn.to_vec()];
    let quic = QuicServerConfig::try_from(tls).map_err(|e| NetError::QuicConfig(e.to_string()))?;
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(quic));
    cfg.transport_config(transport);
    Ok(cfg)
}

/// Dialer configuration: presents `id`, accepts only `peer`.
pub fn client_config(id: &Identity, peer: Fingerprint) -> Result<quinn::ClientConfig, NetError> {
    build_client(
        id,
        Arc::new(PinnedServerVerifier::new(peer)),
        ALPN,
        session_transport(),
    )
}

/// Listener configuration: presents `id`, requires and accepts only `peer`.
pub fn server_config(id: &Identity, peer: Fingerprint) -> Result<quinn::ServerConfig, NetError> {
    build_server(
        id,
        Arc::new(PinnedClientVerifier::new(peer)),
        ALPN,
        session_transport(),
    )
}

/// Pairing dialer: presents `id`, accepts any server certificate.
pub(crate) fn pairing_client_config(id: &Identity) -> Result<quinn::ClientConfig, NetError> {
    build_client(
        id,
        Arc::new(AnyServerCert {
            provider: provider(),
        }),
        PAIR_ALPN,
        pairing_transport(),
    )
}

/// Pairing listener: presents `id`, requires any client certificate.
pub(crate) fn pairing_server_config(id: &Identity) -> Result<quinn::ServerConfig, NetError> {
    build_server(
        id,
        Arc::new(AnyClientCert {
            provider: provider(),
        }),
        PAIR_ALPN,
        pairing_transport(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> UnixTime {
        UnixTime::now()
    }

    #[test]
    fn server_verifier_accepts_only_the_pinned_cert() {
        let pinned = Identity::generate().unwrap();
        let other = Identity::generate().unwrap();
        let v = PinnedServerVerifier::new(pinned.fingerprint());
        let name = ServerName::try_from("macwin").unwrap();
        assert!(
            v.verify_server_cert(pinned.cert(), &[], &name, &[], now())
                .is_ok()
        );
        let err = v
            .verify_server_cert(other.cert(), &[], &name, &[], now())
            .unwrap_err();
        assert_eq!(
            err,
            rustls::Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure)
        );
        assert!(!v.supported_verify_schemes().is_empty());
    }

    #[test]
    fn client_verifier_accepts_only_the_pinned_cert() {
        let pinned = Identity::generate().unwrap();
        let other = Identity::generate().unwrap();
        let v = PinnedClientVerifier::new(pinned.fingerprint());
        assert!(v.offer_client_auth() && v.client_auth_mandatory());
        assert!(v.root_hint_subjects().is_empty());
        assert!(v.verify_client_cert(pinned.cert(), &[], now()).is_ok());
        assert!(v.verify_client_cert(other.cert(), &[], now()).is_err());
    }

    #[test]
    fn configs_build() {
        let me = Identity::generate().unwrap();
        let peer = Identity::generate().unwrap();
        assert!(client_config(&me, peer.fingerprint()).is_ok());
        assert!(server_config(&me, peer.fingerprint()).is_ok());
        assert!(pairing_client_config(&me).is_ok());
        assert!(pairing_server_config(&me).is_ok());
    }

    #[test]
    fn pairing_verifiers_accept_any_certificate() {
        let any = Identity::generate().unwrap();
        let name = ServerName::try_from("macwin").unwrap();
        let server = AnyServerCert {
            provider: provider(),
        };
        assert!(
            server
                .verify_server_cert(any.cert(), &[], &name, &[], now())
                .is_ok()
        );
        let client = AnyClientCert {
            provider: provider(),
        };
        assert!(client.client_auth_mandatory());
        assert!(client.verify_client_cert(any.cert(), &[], now()).is_ok());
    }
}
