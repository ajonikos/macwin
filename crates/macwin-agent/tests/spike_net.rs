//! `spike net` listener and dialer in-process on 127.0.0.1.

use std::net::SocketAddr;
use std::time::Duration;

use macwin_agent::spike_net::{SpikeOpts, run_dialer, run_listener, spike_hello};
use macwin_core::types::Os;
use macwin_net::{Identity, connect, listen};

#[tokio::test(flavor = "multi_thread")]
async fn both_paths_measure_every_ping() {
    let local: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let a = Identity::generate().unwrap();
    let b = Identity::generate().unwrap();
    let listener = listen(local, &b, a.fingerprint()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let session = listener
            .accept(&spike_hello(Os::Windows, "pc"))
            .await
            .unwrap();
        run_listener(session).await
    });
    let session = connect(
        local,
        addr,
        &a,
        b.fingerprint(),
        &spike_hello(Os::MacOs, "mac"),
    )
    .await
    .unwrap();
    let opts = SpikeOpts {
        count: 50,
        interval: Duration::from_millis(1),
    };
    let report = tokio::time::timeout(Duration::from_secs(20), run_dialer(session, opts))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (report.stream.sent, report.stream.received),
        (50, 50),
        "{report}"
    );
    assert_eq!(
        (report.datagram.sent, report.datagram.received),
        (50, 50),
        "{report}"
    );
    assert!(report.stream.p50 <= report.stream.p99 && report.stream.p99 <= report.stream.max);
    // The listener ends cleanly when the dialer closes normally.
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
