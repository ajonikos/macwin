//! The `macwin` binary end to end, each child with its own config dir.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_macwin");

fn macwin(config: &Path) -> Command {
    let mut c = Command::new(BIN);
    c.env("MACWIN_CONFIG_DIR", config).env("RUST_LOG", "warn");
    c
}

fn fingerprint_of(config: &Path) -> String {
    let out = macwin(config).arg("identity").output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("fingerprint: "))
        .expect("fingerprint line")
        .to_string()
}

/// Kills the child when dropped (also when a test fails).
struct Guard(Child);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_with_timeout(mut child: Guard, timeout: Duration) -> Output {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(_status) = child.0.try_wait().unwrap() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the child did not exit within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut stdout = String::new();
    let mut stderr = String::new();
    use std::io::Read;
    if let Some(mut s) = child.0.stdout.take() {
        s.read_to_string(&mut stdout).unwrap();
    }
    if let Some(mut s) = child.0.stderr.take() {
        s.read_to_string(&mut stderr).unwrap();
    }
    let status = child.0.wait().unwrap();
    Output {
        status,
        stdout: stdout.into_bytes(),
        stderr: stderr.into_bytes(),
    }
}

#[test]
fn identity_is_created_once_and_stable() {
    let dir = tempfile::tempdir().unwrap();
    let first = macwin(dir.path()).arg("identity").output().unwrap();
    assert!(first.status.success());
    assert!(String::from_utf8_lossy(&first.stderr).contains("created a new identity"));
    let second = macwin(dir.path()).arg("identity").output().unwrap();
    assert!(!String::from_utf8_lossy(&second.stderr).contains("created"));
    let fp = |o: &Output| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(fp(&first), fp(&second));
    let line = fp(&first);
    let hex = line.strip_prefix("fingerprint: ").unwrap();
    assert_eq!(hex.len(), 64);
    assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(
        String::from_utf8_lossy(&first.stdout)
            .contains(&format!("config dir: {}", dir.path().display()))
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("identity.key.pem"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

/// Starts a listener and returns it with its bound address.
fn start_listener(config: &Path, peer_fp: &str) -> (Guard, String) {
    let mut child = Guard(
        macwin(config)
            .args([
                "spike",
                "net",
                "--listen",
                "127.0.0.1:0",
                "--peer-fp",
                peer_fp,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(addr) = line.strip_prefix("listening on ") {
                let _ = tx.send(addr.to_string());
            }
        }
    });
    let addr = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("listener address");
    (child, addr)
}

#[test]
fn spike_net_between_two_processes() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let (fp_a, fp_b) = (fingerprint_of(a.path()), fingerprint_of(b.path()));
    let (listener, addr) = start_listener(b.path(), &fp_a);
    let dialer = Guard(
        macwin(a.path())
            .args([
                "spike",
                "net",
                "--connect",
                &addr,
                "--peer-fp",
                &fp_b,
                "--count",
                "20",
                "--interval-ms",
                "1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let out = wait_with_timeout(dialer, Duration::from_secs(30));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "dialer failed: {stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for path in ["stream", "datagram"] {
        let line = stdout
            .lines()
            .find(|l| l.starts_with(path))
            .unwrap_or_else(|| panic!("no {path} line in {stdout}"));
        for field in ["n=20", "lost=0", "p50=", "p95=", "p99=", "max="] {
            assert!(line.contains(field), "{field} missing in {line:?}");
        }
    }
    // The listener exits on its own after the normal close.
    let out = wait_with_timeout(listener, Duration::from_secs(10));
    assert!(out.status.success(), "listener exit: {:?}", out.status);
}

#[test]
fn dialer_with_a_wrong_pin_fails_quickly() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let fp_a = fingerprint_of(a.path());
    let (_listener, addr) = start_listener(b.path(), &fp_a);
    let wrong = "00".repeat(32);
    let started = Instant::now();
    let dialer = Guard(
        macwin(a.path())
            .args([
                "spike",
                "net",
                "--connect",
                &addr,
                "--peer-fp",
                &wrong,
                "--count",
                "5",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let out = wait_with_timeout(dialer, Duration::from_secs(10));
    assert!(
        !out.status.success(),
        "the dialer must refuse an unpinned server"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(String::from_utf8_lossy(&out.stderr).contains("error:"));
}
