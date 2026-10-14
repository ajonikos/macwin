# macwin Phase 0a ("foundation") — Implementation Plan

Binding inputs, in priority order: (1) the user's messages + the Phase 0a brief, (2) `/Users/kulacidmyt/Documents/macwin/.agents/tasks/design.md` §0–§5.10, (3) the gap-filling decisions in section B of this plan. design.md is TRUNCATED: it ends inside §5.10 at `<!-- PART3 -->`. Its references to §6–§20 (proto, net, platform traits, agent, CI, deny.toml, §16 validation, §20 open items) point at text that was never written. Section B below fills those parts. Do NOT plan or build Phase 0b (real OS backends, `spike kvm/capture/clipboard`, `scripts/macos/*`, bundling).

## A. Ground rules (apply to every item)

- Work directly in `/Users/kulacidmyt/Documents/macwin` using absolute paths. No worktree.
- NO git commits, NO pushes, no git config changes.
- Every cargo command uses `RUSTUP_TOOLCHAIN=stable` (stable is exactly 1.96.0; the pinned `1.96.0` toolchain is not installed and must not be auto-installed). Shorthand used below: `C=RUSTUP_TOOLCHAIN=stable cargo`.
- Safety: nothing captures or injects input, touches the clipboard, or triggers a permission prompt. Every socket in tests/spikes binds `127.0.0.1` (never `0.0.0.0`). No `brew install`, no `cargo install`, no rustup changes. Allowed network: crates.io downloads and the one-time Chromium fetch in item 2. (The Chromium LICENSE file is fetched from the same googlesource host in the same script; it is needed verbatim for THIRD_PARTY_NOTICES. Flag this in the final report.)
- Tests never mutate the process environment (`std::env::set_var` is unsafe in edition 2024). Child processes get env via `Command::env`.
- Only markdown files allowed: this plan, `README.md`, the final report. `THIRD_PARTY_NOTICES` has no extension.
- Code/comments in English. `#![forbid(unsafe_code)]` in core, proto, net, platform, agent. Key identities (HID codes) are never logged above TRACE; fingerprints may be logged.
- Emergency chord discrepancy: the brief says Ctrl+Alt/Option+Shift+**Q**, design §5.8 says Ctrl+Alt+Shift+**Esc**. Implement the design's Esc chord as written, add a one-line `// NOTE:` in `safety.rs` citing the discrepancy, record it in FEAT findings / the final report, and let the reviewer flag it.
- `cargo-deny` is not installed and must not be installed. deny.toml is validated locally by the license audit in item 6 and for real in CI.

Full verification sweep ("V-ALL"), run from `/Users/kulacidmyt/Documents/macwin`:

```
export RUSTUP_TOOLCHAIN=stable
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo run -q -p keymap-gen -- --check
cargo clippy --workspace --all-targets --all-features --target x86_64-pc-windows-gnu -- -D warnings
cargo build --workspace --target x86_64-pc-windows-gnu
cargo test --workspace --all-features --no-run --target x86_64-pc-windows-gnu
cargo tree --workspace --all-features --target all -i aws-lc-rs   # must fail: "package ID specification `aws-lc-rs` did not match any packages"
```

## B. Gap-filling design decisions (binding for this phase)

### B1. Workspace

- Root `Cargo.toml`: `[workspace] members = ["crates/*", "tools/keymap-gen"]`, `resolver = "3"`; `[workspace.package]`, `[workspace.lints]`, `[profile.release]` exactly as design §4. `[workspace.dependencies]` = design §3 table with `=x.y.z` pins and the listed features/default-features, plus internal path deps (`macwin-core = { path = "crates/macwin-core" }`, etc.). Each crate: `[lints] workspace = true`, deps via `{ workspace = true }`.
- OS crates declare their FFI deps now (under `[target.'cfg(target_os = "macos")'.dependencies]` / `[target.'cfg(windows)'.dependencies]`) so Cargo.lock, the Windows cross-build and cargo-deny cover the real graph. Their `lib.rs` is only the cfg gate + crate doc in 0a.
- `macwin-agent` is lib + bin: `[lib] name = "macwin_agent", path = "src/lib.rs"` and `[[bin]] name = "macwin", path = "src/main.rs"`. The session wiring lives in the lib so Phase 0b's `spike kvm` reuses it. Features: `default = ["private-cursor-api"]`, `private-cursor-api = ["macwin-platform-macos/private-cursor-api"]`; `macwin-platform-macos` has `[features] private-cursor-api = []`. Dev-deps: `macwin-platform` with `features = ["testing"]`, `tempfile`.
- Add `.gitattributes` with `* text=auto eol=lf` so Windows CI checkouts keep LF (otherwise `keymap-gen --check` and the golden data would differ).

### B2. macwin-proto (design §5.2 messages, prost hand-written derives, no protoc)

Files: `src/lib.rs` (pub API, `ProtoError`, version), `src/wire.rs` (prost types), `src/convert.rs` (wire <-> domain with validation), `src/frame.rs` (stream framing), `tests/golden.rs`.

Wire schema (proto3 semantics; prost derives `Debug` itself — never add `#[derive(Debug)]` to Message/Oneof types):

```
ControlFrame  { oneof kind { Hello hello = 1; Heartbeat heartbeat = 2; Ping ping = 3; Pong pong = 4; } }
InputFrame    { oneof kind { Enter enter = 1; Leave leave = 2; TakeControl take_control = 3; Key key = 4;
                             Button button = 5; Wheel wheel = 6; ModifierSync modifier_sync = 7; Ping ping = 8; Pong pong = 9; } }
DatagramFrame { oneof kind { MouseAbs mouse_abs = 1; Ping ping = 2; Pong pong = 3; } }
Hello   { uint32 protocol_version = 1; repeated string capabilities = 2; Os os = 3; string os_version = 4;
          string device_name = 5; repeated Screen screens = 6; Side peer_side = 7; string agent_version = 8; }
Screen  { uint32 id = 1; sint32 x = 2; sint32 y = 3; uint32 w = 4; uint32 h = 5; double native_per_logical = 6; bool primary = 7; }
Point   { sint32 x = 1; sint32 y = 2; }
Heartbeat { uint64 epoch = 1; Role role = 2; }
Ping    { uint64 id = 1; uint64 sent_micros = 2; }      Pong { uint64 id = 1; uint64 ping_sent_micros = 2; }
Enter   { uint64 epoch = 1; Point at = 2; }  Leave { uint64 epoch = 1; }  TakeControl { uint64 epoch = 1; }
Key     { uint32 hid = 1; bool down = 2; bool repeat = 3; }
Button  { MouseButton button = 1; bool down = 2; Point at = 3; }
Wheel   { sint32 dx = 1; sint32 dy = 2; WheelUnit unit = 3; }
ModifierSync { uint32 mask = 1; }
MouseAbs { uint64 epoch = 1; uint64 seq = 2; Point at = 3; }
enum Os { OS_UNSPECIFIED = 0; OS_MACOS = 1; OS_WINDOWS = 2; }
enum Side { SIDE_UNSPECIFIED = 0; SIDE_LEFT = 1; SIDE_RIGHT = 2; }          // 0 => peer_side None
enum Role { ROLE_UNSPECIFIED = 0; ROLE_IDLE = 1; ROLE_CONTROLLING = 2; ROLE_CONTROLLED = 3; }
enum MouseButton { MB_UNSPECIFIED = 0; MB_LEFT = 1; MB_RIGHT = 2; MB_MIDDLE = 3; MB_BACK = 4; MB_FORWARD = 5; }
enum WheelUnit { WU_UNSPECIFIED = 0; WU_NOTCH120 = 1; WU_PIXEL = 2; }
```

Golden (design §2): `InputFrame{Key{hid: 0x070004, down: true}}` encodes to `22 06 08 84 80 1c 10 01`.

Public API (`lib.rs`):

```rust
pub const PROTOCOL_VERSION: u32 = 0x0001_0000;          // major << 16 | minor
pub const MAX_FRAME_LEN: usize = 64 * 1024;             // body bytes, streams and datagrams
pub fn version_major(v: u32) -> u16;
pub fn check_version(peer: u32) -> Result<(), ProtoError>;   // Err(IncompatibleVersion) if peer == 0 or major differs
pub fn encode_control(m: &ControlMsg) -> Vec<u8>;  pub fn decode_control(b: &[u8]) -> Result<ControlMsg, ProtoError>;
pub fn encode_input(m: &InputMsg) -> Vec<u8>;      pub fn decode_input(b: &[u8]) -> Result<InputMsg, ProtoError>;
pub fn encode_datagram(m: &DatagramMsg) -> Vec<u8>; pub fn decode_datagram(b: &[u8]) -> Result<DatagramMsg, ProtoError>;
pub mod frame { pub fn encode(body: &[u8]) -> bytes::Bytes;   // varint length prefix + body
                pub struct FrameBuffer; impl FrameBuffer { pub fn new() -> Self; pub fn push(&mut self, chunk: &[u8]);
                pub fn next_frame(&mut self) -> Result<Option<bytes::Bytes>, ProtoError>; } }  // Err(FrameTooLarge) > MAX_FRAME_LEN
#[derive(thiserror::Error, Debug)] pub enum ProtoError { Decode(#[from] prost::DecodeError), MissingField(&'static str),
  UnknownKind(&'static str), InvalidEnum { field: &'static str, value: i32 }, InvalidValue { field: &'static str, reason: &'static str },
  FrameTooLarge { len: usize }, IncompatibleVersion { local: u32, peer: u32 } }
```

Validation in `convert.rs` ("parse, don't validate"): missing oneof -> `UnknownKind`; missing required submessage (`at`) -> `MissingField`; enum value 0 or unknown -> `InvalidEnum` (except `peer_side` 0 = None); Hid page must be 0x07 or 0x0C and `hid <= 0x00FF_FFFF`; `ModifierSync.mask <= 0xFF`; screens: 1..=16 entries, unique ids, each built via core `Rect::new` (see B3) and `native_per_logical` finite in `[0.25, 8.0]`; strings <= 256 bytes; capabilities <= 32 entries of <= 64 bytes; `WheelUnit::Pixel` accepted by the codec (reserved, Engine ignores). Datagrams: one `DatagramFrame` per datagram, no length prefix.

Tests: golden bytes (`tests/golden.rs`); a reject test per validation rule; `check_version` (same major other minor OK, other major / 0 rejected); `FrameBuffer` (split chunks, several frames in one chunk, oversize length rejected before allocation); proptest round trip `decode(encode(m)) == m` for all three message families using generated valid domain values; proptest "arbitrary bytes never panic" for the three decoders and `FrameBuffer`.

### B3. macwin-core additions not spelled out in design

- `Rect::new(x, y, w, h) -> Result<Rect, GeometryError>`: `w, h` in `1..=65_536`, `|x|, |y| <= 1 << 20`; accessors `left/top/right/bottom/width/height` (right/bottom exclusive). `GeometryError` via thiserror. Screen sets: non-empty, <= 16, unique ids; overlaps are allowed.
- `Timings` struct with fields `tick, heartbeat_interval, heartbeat_timeout, grab_lease, takeover_threshold: f64, takeover_window` and `Default` = design constants.
- Module layout exactly as design §1 (`types.rs, msg.rs, geometry.rs, layout.rs, remap.rs, keys.rs, heartbeat.rs, safety.rs, engine/{mod.rs, session.rs}, keymap/{mod.rs, generated.rs}, os/{mod.rs, macos.rs, windows.rs}`). Unit tests in `#[cfg(test)] mod tests` per file; engine scenario tests may live in `engine/tests.rs` (declared `#[cfg(test)] mod tests;`).

### B4. macwin-platform (traits + fakes)

`src/lib.rs`:

```rust
#[derive(thiserror::Error, Debug)] pub enum PlatformError { #[error("unsupported on this OS")] Unsupported,
  #[error("permission denied: {0}")] PermissionDenied(String), #[error("backend error: {0}")] Backend(String) }
/// Called synchronously from the capture thread. Must not block for long.
pub trait CaptureSink: Send + Sync + 'static {
    fn on_event(&self, ev: CapturedEvent) -> bool;        // true = deliver locally, false = swallow
    fn on_fault(&self, fault: CaptureFault);
}
/// Must not block; callable from inside CaptureSink::on_event (re-entrant).
pub trait CaptureControl: Send + Sync {
    fn command(&self, cmd: GrabCommand);
    fn renew_lease(&self, until: std::time::Instant);
    fn stop(&self);
}
pub trait InputCapture: Send { fn start(&mut self, sink: Arc<dyn CaptureSink>) -> Result<Arc<dyn CaptureControl>, PlatformError>; }
pub trait InputInjector: Send { fn inject(&mut self, cmd: InjectCommand) -> Result<(), PlatformError>; }
pub trait ScreenSource: Send { fn screens(&self) -> Result<Vec<ScreenInfo>, PlatformError>; }
pub struct UnsupportedPlatform;   // implements all three; every method returns Err(Unsupported)
#[cfg(feature = "testing")] pub mod testing;
```

No clipboard trait in 0a (clipboard is Phase 0b/1; nothing here touches it).

`src/testing.rs` (feature `testing`), all thread-safe via `std::sync::Mutex`, handles are `Clone`:
- `FakeCapture::new() -> (FakeCapture, FakeCaptureHandle)`. `start` stores the sink, returns an `Arc<dyn CaptureControl>` that records commands. Handle: `emit(ev) -> bool` (panics if not started; clones the sink out of the lock, then calls it with NO internal lock held — the sink re-enters `command()`), `emit_fault(f)`, `commands() -> Vec<GrabCommand>`, `grabbed() -> bool` (last Grab not followed by Release), `lease() -> Option<Instant>`, `stopped() -> bool`.
- `FakeInjector::new() -> (FakeInjector, FakeInjectorHandle)`. Handle: `commands() -> Vec<InjectCommand>`, `pressed_keys() -> BTreeSet<Hid>`, `pressed_buttons() -> Vec<MouseButton>`, `last_pos() -> Option<Point>`, `clear()`. Key up for a key not pressed is recorded (and counted by `unmatched_ups()`), not a panic.
- `FakeScreens(pub Vec<ScreenInfo>)`.
Unit tests in `testing.rs` cover re-entrancy (sink calling `command()` during `emit`) and pressed-state bookkeeping.

### B5. macwin-net

Files per design §1: `identity.rs, fingerprint.rs, tls.rs, endpoint.rs, session.rs, link.rs, codes.rs`, `tests/loopback.rs`. Errors: `NetError` (thiserror) with variants `Io, Tls(rustls::Error), Connect(quinn::ConnectError), Connection(quinn::ConnectionError), Proto(ProtoError), HelloTimeout, StreamSetup(&'static str), Identity(String), InvalidFingerprint(String), ConfigDir(String)`.

- `fingerprint.rs`: `pub struct Fingerprint(pub [u8; 32])`; `Fingerprint::of_cert(der: &[u8])` = SHA-256 via `ring::digest`; `Display` = 64 lowercase hex chars; `FromStr` accepts 64 hex digits case-insensitively with optional `:` separators, anything else -> `InvalidFingerprint`.
- `identity.rs`: `pub struct Identity { cert: CertificateDer<'static>, key: PrivatePkcs8KeyDer<'static> }`; `Identity::generate()` (rcgen `generate_simple_self_signed(vec!["macwin".into()])`, ECDSA P-256 via ring; fields `cert` / `signing_key`), `fingerprint()`, `load_or_create(dir) -> Result<(Identity, bool /*created*/), NetError>`. Files `identity.crt.pem` + `identity.key.pem`. Unix: dir created with 0700, files written to a temp name with mode 0600 (`OpenOptionsExt::mode`) then renamed. Exactly one file present -> `Identity` error ("incomplete identity in <dir>; remove it to regenerate"), never silent regeneration. Load via `rustls::pki_types::pem::PemObject`. `pub fn config_dir(env: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, NetError>`: non-empty `MACWIN_CONFIG_DIR` wins; macOS `$HOME/Library/Application Support/macwin`; Windows `%APPDATA%\macwin`; other unix `$XDG_CONFIG_HOME/macwin` or `$HOME/.config/macwin`. Tests use tempfile dirs and closures.
- `tls.rs`: provider = `Arc::new(rustls::crypto::ring::default_provider())`, always passed via `builder_with_provider`, TLS 1.3 only, ALPN `b"macwin/1"`. `PinnedServerVerifier { expected: Fingerprint, provider }` implements `rustls::client::danger::ServerCertVerifier`: `verify_server_cert` hashes the end-entity cert and returns `Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure)` on mismatch (server name and intermediates ignored); `verify_tls12_signature` returns an error (TLS 1.2 disabled); `verify_tls13_signature` delegates to `rustls::crypto::verify_tls13_signature(.., &provider.signature_verification_algorithms)`; `supported_verify_schemes` from the provider. `PinnedClientVerifier` implements `rustls::server::danger::ClientCertVerifier` the same way with `offer_client_auth() = true`, `client_auth_mandatory() = true`, `root_hint_subjects() = &[]`. Mismatches log WARN with expected and actual fingerprints. `pub fn client_config(id, peer_fp) -> quinn::ClientConfig`, `pub fn server_config(id, peer_fp) -> quinn::ServerConfig` via `quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig}::try_from`. Transport config: `max_idle_timeout = 10 s`, `keep_alive_interval = 1 s`, datagrams enabled (default receive buffer). There is no code path that builds a config without the pinned verifiers.
- `codes.rs`: QUIC application close codes `NORMAL = 0, PROTOCOL_VIOLATION = 1, VERSION_MISMATCH = 2, HEARTBEAT_TIMEOUT = 3, HELLO_TIMEOUT = 4, INTERNAL = 5`; `fn code_for(&CloseReason) -> VarInt`.
- `endpoint.rs`: `pub async fn listen(bind: SocketAddr, id: &Identity, peer_fp: Fingerprint) -> Result<Listener, NetError>`; `Listener::local_addr()`, `Listener::accept(&self, hello: &Hello) -> Result<Session, NetError>` (waits for the next incoming connection and returns its established Session or its error; caller decides whether to keep accepting). `pub async fn connect(bind: SocketAddr, remote: SocketAddr, id: &Identity, peer_fp: Fingerprint, hello: &Hello) -> Result<Session, NetError>`; SNI `"macwin"`. Callers pass `bind` explicitly (tests: `127.0.0.1:0`; agent: loopback if the remote is loopback, else unspecified of the same family).
- `session.rs` (establishment): dialer opens two bi streams and writes a 1-byte header on each (`0x01` control, `0x02` input) so the listener's `accept_bi` returns; listener accepts two bi streams and assigns them by header (wrong/duplicate header -> close `PROTOCOL_VIOLATION`). Input send stream `set_priority(1)`, control `0`. Both sides immediately send `Hello` on control, then read the peer's first control frame within `HELLO_TIMEOUT = 5 s`: not a Hello -> close `PROTOCOL_VIOLATION`; `check_version` fails -> close `VERSION_MISMATCH`, return `Proto(IncompatibleVersion)`; timeout -> close `HELLO_TIMEOUT`. Note: with TLS 1.3 a client whose certificate the server rejects may finish its handshake first; it then fails during stream setup / Hello read. `Session { pub peer: Hello, pub link: Link, pub remote: SocketAddr }` and keeps the `Endpoint` alive.
- `link.rs` (transport-agnostic, consumed by the agent driver):

```rust
pub enum Inbound { Control(ControlMsg), Input(InputMsg), Datagram(DatagramMsg), Closed(LinkClosed) }
pub enum LinkClosed { PeerClosed { code: u64 }, LocalClosed { code: u64 }, TimedOut, Transport(String), ProtocolViolation(String) }
#[derive(Clone)] pub struct LinkSender { /* bounded mpsc senders */ }
impl LinkSender { pub fn send_control(&self, m: ControlMsg) -> Result<(), LinkError>;   // non-blocking try_send
                  pub fn send_input(&self, m: InputMsg) -> Result<(), LinkError>;
                  pub fn send_datagram(&self, m: DatagramMsg);                        // drops when full (latest wins)
                  pub fn close(&self, code: u64);       // graceful: flush+finish streams (bounded 500 ms), then close
                  pub fn close_now(&self, code: u64); }  // immediate connection close
pub struct Link { pub tx: LinkSender, pub rx: tokio::sync::mpsc::Receiver<Inbound> }
impl Link { pub fn memory_pair() -> (Link, Link); }   // in-process transport for driver unit tests
```

Channels: control/input capacity 4096; `try_send` Full on control/input -> `close_now(INTERNAL)` and `Err(LinkError::Overflow)`; datagram capacity 256. QUIC impl: one writer task per stream (encode with proto + `frame::encode`, `write_all`), one datagram task (`connection.send_datagram`, oversize/blocked -> debug log, drop), one reader task per stream (`read_chunk` -> `FrameBuffer` -> decode -> `Inbound`), one datagram reader (`read_datagram`). Any decode error on any channel -> `close_now(PROTOCOL_VIOLATION)` and `Inbound::Closed(ProtocolViolation)`. Connection end -> exactly one `Inbound::Closed(..)` mapped from `quinn::ConnectionError` (ApplicationClosed -> PeerClosed{code}, LocallyClosed -> LocalClosed, TimedOut -> TimedOut, else Transport). Heartbeat logic is NOT in net: the Engine (core) generates and checks heartbeats; `spike net` uses `macwin_core::heartbeat::HeartbeatMonitor` directly.

`tests/loopback.rs` (all on 127.0.0.1, every await wrapped in `tokio::time::timeout(5 s)`, `#[tokio::test(flavor = "multi_thread")]`):
1. correct pins: Hello exchanged both ways with equal fields; control, input and datagram messages + Ping/Pong cross both directions;
2. client pins wrong server fp -> `connect` Err and server `accept` Err;
3. server pins wrong client fp -> `accept` Err and `connect` Err;
4. major-version mismatch in the dialer's Hello -> both sides `Err(Proto(IncompatibleVersion))`;
5. graceful `close(NORMAL)` after 100 input messages -> peer receives all 100 in order, then `Closed(PeerClosed{code: 0})`;
6. `close_now(INTERNAL)` -> peer `Closed(PeerClosed{code: 5})`;
7. `Link::memory_pair` round trip;
8. identity: `load_or_create` creates once and reloads the same fingerprint; files 0600 and dir 0700 (`#[cfg(unix)]`); half-present identity -> error; fingerprint parse/format tests in unit tests.

### B6. Session wiring (agent library `crates/macwin-agent/src/driver.rs`)

```rust
pub struct SessionConfig { pub local_os: Os, pub os_version: String, pub device_name: String,
                           pub peer_side: Side, pub remap: bool, pub timings: Timings }
pub struct Backends { pub capture: Box<dyn InputCapture>, pub injector: Box<dyn InputInjector>, pub screens: Box<dyn ScreenSource> }
pub fn kvm_hello(cfg: &SessionConfig, screens: Vec<ScreenInfo>) -> Hello;   // caps: input.capture + input.inject, PROTOCOL_VERSION
pub fn peer_info(h: &Hello) -> PeerInfo;
pub struct Connected { pub link: Link, pub peer: Hello, pub role: TransportRole }
pub enum SessionEnd { PeerClosed, HeartbeatTimeout, ProtocolViolation(String), Shutdown, Transport(String), Internal(String) }
pub struct SessionHandle { pub role: tokio::sync::watch::Receiver<RoleKind>, /* shutdown tx */ join: JoinHandle<SessionEnd> }
impl SessionHandle { pub fn shutdown(&self); pub async fn join(self) -> SessionEnd; }
pub fn spawn_session(cfg: SessionConfig, backends: Backends, conn: Connected) -> Result<SessionHandle, PlatformError>;
```

Mechanics: one `Arc<Mutex<Core>>` where `Core { engine: Engine, injector, capture: Arc<dyn CaptureControl>, link: LinkSender, role_tx, end: Option<SessionEnd> }`. `spawn_session` reads screens, builds the Engine (`EngineConfig` from cfg + screens), starts capture with a `CaptureSink` that locks Core, calls `on_local_event(Instant::now(), ev)`, dispatches actions, returns `deliver`; `on_fault` -> `on_capture_fault`. Then `on_connected(now, peer_info(&peer), role)`. Async loop (`tokio::select!`): `link.rx.recv()` -> `on_control/on_input/on_datagram`; `Inbound::Closed` -> `on_disconnected`, end = mapped reason; `tokio::time::interval(timings.tick)` (MissedTickBehavior::Delay) -> `on_tick`; shutdown signal -> `engine.shutdown`. `dispatch(actions)` executes in order: Send* -> LinkSender (errors end the session as Internal), Inject -> injector (errors logged WARN), Capture -> `capture.command`, RenewLease -> `capture.renew_lease`, Close(reason) -> `link.close(code_for(reason))` and set `end`. Lock order is always Core -> fake/backend internals; nothing awaits while holding the std Mutex. After every dispatch, publish `engine.role()` to `role_tx`. Loop exits when `end` is set; on exit `capture.stop()`. The driver depends only on `Link`, never on quinn types.

`tests/e2e_fake.rs` (127.0.0.1, real QUIC via macwin-net, fakes from `macwin-platform/testing`, `wait_until(5 s, cond)` polling helper at 10 ms, `Identity::generate()` per peer): A = Mac 1440x900 npl 1.0, peer_side Right, dialer; B = Windows 1920x1080 npl 1.0, peer_side Left, listener.
1. Hello exchange: each side's `Connected.peer` matches the other's `kvm_hello`.
2. Crossing A -> B: A emits MouseMove at (1439.5, 450) -> A role Controlling, A capture `Grab{exit: (1439,450)}`, B injector `MoveTo((0,540))`; a delta move reaches B as MoveTo via datagram; A LeftGUI down/up -> B LeftCtrl down/up (remap).
3. Crossing B -> A from B's left edge (symmetric assertions).
4. TakeControl: while A controls B, B emits a physical key down -> B delivers locally (emit returns true), B role Idle, A capture receives `Release{warp_to: (1437, 450)}`, A role Idle.
5. Disconnect: raw `macwin-net` Session as A (no driver) sends `Enter{epoch: 1}`, Key down (LeftShift), Button down; B injector shows both held; A `close_now(INTERNAL)` -> B injector pressed sets empty, B `SessionEnd::PeerClosed`.
6. Heartbeat timeout: raw A sends Enter + Key down then goes silent (connection kept open) -> within 1.5–3 s B releases everything and ends with `SessionEnd::HeartbeatTimeout`.
7. Shutdown: A controlling with a key held, `A.shutdown()` -> B receives Leave and releases; both sessions end.
(Wrong-fingerprint rejection both sides is covered in net's loopback tests and again at CLI level in item 16.)

### B7. Agent CLI (`crates/macwin-agent/src/{main.rs, cli.rs, logging.rs, spike_net.rs}`)

- `logging.rs`: tracing-subscriber fmt to stderr, `EnvFilter` from `RUST_LOG`, default `info`.
- `cli.rs` (clap derive): `macwin identity`; `macwin spike net (--listen <ADDR:PORT> | --connect <HOST:PORT>) --peer-fp <HEX> [--count <N>=1000] [--interval-ms <MS>=2]` (ArgGroup required + exclusive). Clap unit test via `Cli::command().debug_assert()` and parse tests.
- `identity`: `load_or_create(config_dir(|k| std::env::var_os(k)))`; stdout: `fingerprint: <hex>` and `config dir: <path>`; stderr "created new identity" when created.
- `spike net`: loads/creates identity, prints `local fingerprint: <hex>`. Hello caps `[spike.net]`, `os` from cfg (`macos`/`windows`; other hosts -> error "unsupported OS"), `device_name` from `COMPUTERNAME`/`HOSTNAME` env or `"unknown"`. Listener prints `listening on <addr>` (flushed) and keeps accepting until one session is established (handshake errors logged WARN); answers every Ping on the channel it arrived on (Ping on control/input/datagram -> Pong on the same channel); exits 0 when the peer closes with NORMAL. Dialer resolves the host with `tokio::net::lookup_host`, measures the stream path (Ping on the input stream) then the datagram path, `count` pings each at `interval-ms`, `sent_micros` from a process-local `Instant` anchor, pongs matched by id, missing pongs 2 s after the last send counted as lost; prints `stream   n=.. lost=.. p50=..ms p95=..ms p99=..ms max=..ms` and the same for `datagram`, then `close(NORMAL)`, exit 0. Both sides send Heartbeat every 250 ms and abort with an error if `HeartbeatMonitor::timed_out` (1.5 s). Percentiles: nearest-rank `samples[ceil(p/100 * n) - 1]` on sorted durations, in a pure `LatencyStats::from_samples` with unit tests. Library entry points `spike_net::run_listener(Session)` / `run_dialer(Session, SpikeOpts) -> Report` so tests run in-process.
- `main.rs`: `fn main() -> ExitCode`, `#[tokio::main]` multi-thread runtime, errors printed via anyhow `{:#}`, exit 1.

### B8. deny.toml, notices, CI

- `deny.toml` (cargo-deny v2 format): `[graph] all-features = true, targets = ["aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-pc-windows-gnu", "x86_64-pc-windows-msvc"]`; `[licenses] version = 2, allow = ["MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "ISC", "BSD-3-Clause", "Zlib", "Unicode-3.0"], confidence-threshold = 0.93, private = { ignore = true }` with a comment that everything not allowed (GPL, AGPL, LGPL, …) is denied; `[bans] multiple-versions = "warn", wildcards = "deny", allow-wildcard-paths = true`; `[advisories] version = 2`; `[sources] unknown-registry = "deny", unknown-git = "deny"`. Add `"Unlicense"`/`"BSD-2-Clause"` only if the item-6 audit shows a package whose expression cannot be satisfied otherwise.
- `THIRD_PARTY_NOTICES`: section for Chromium `ui/events/keycodes/dom/dom_code_data.inc` (what was derived, where: `crates/macwin-core/src/keymap/generated.rs`, source URL) followed by the verbatim Chromium LICENSE text; one line noting Rust crate dependencies carry their own licenses (enforced by deny.toml).
- `.github/workflows/ci.yml`: `on: push (branches master, main) + pull_request`, `permissions: contents: read`, `env: CARGO_TERM_COLOR: always`. Job `test` matrix `os: [macos-15, windows-2025]`, `fail-fast: false`: `actions/checkout@v6.0.2`; `rustup toolchain install 1.96.0 --profile minimal -c rustfmt,clippy`; `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`; `cargo test --workspace --all-features --locked`; `cargo run -p keymap-gen --locked -- --check`. Job `deny` on `ubuntu-24.04`: checkout, `EmbarkStudios/cargo-deny-action@v2.0.20` with `command: check`, `rust-version: "1.96.0"`. Deviation to note in the final report: cargo-deny-action is a Docker action and runs only on Linux runners, so cargo-deny runs as a third Linux job that checks the macOS and Windows target graphs via `[graph] targets`; macOS/Windows jobs run fmt/clippy/tests.

## C. Ordered implementation items

### FEAT-001 — workspace skeleton, tooling files, keymap generator and table

- [ ] 1. Create the workspace skeleton: root `Cargo.toml` (B1, design §3/§4), `rust-toolchain.toml` and `.cargo/config.toml` (verbatim from design §4), `.gitattributes` (B1), add `.DS_Store` to `.gitignore`. Create all eight packages with `Cargo.toml` + minimal source: core/proto/net/platform/agent-lib `lib.rs` with `#![forbid(unsafe_code)]` and a crate doc; `macwin-platform-macos/src/lib.rs` = `#![cfg(target_os = "macos")]` + doc; `macwin-platform-windows/src/lib.rs` = `#![cfg(windows)]` + doc; agent `main.rs` = `fn main() {}` placeholder; `tools/keymap-gen/src/main.rs` placeholder. Declare every dependency each crate will need (design §3 "Used by" column, B1 dev-deps, platform `testing` feature, agent features). Cargo.lock is generated and kept.
      Files: Cargo.toml, Cargo.lock, rust-toolchain.toml, .cargo/config.toml, .gitattributes, .gitignore, crates/*/Cargo.toml, crates/*/src/lib.rs, crates/macwin-agent/src/main.rs, tools/keymap-gen/{Cargo.toml,src/main.rs}
      Verify: `RUSTUP_TOOLCHAIN=stable cargo build --workspace --all-features` and `RUSTUP_TOOLCHAIN=stable cargo build --workspace --target x86_64-pc-windows-gnu` succeed; `RUSTUP_TOOLCHAIN=stable cargo tree --workspace --all-features --target all -i aws-lc-rs` fails with "did not match any packages"; `cargo tree -i` for `macwin-core` shows no dependency on proto/net.

- [ ] 2. Write `tools/keymap-gen/fetch.sh` (design §5.4: curl `https://chromium.googlesource.com/chromium/src/+/main/ui/events/keycodes/dom/dom_code_data.inc?format=TEXT` | `base64 -D` > `data/dom_code_data.inc`; same for `https://chromium.googlesource.com/chromium/src/+/main/LICENSE?format=TEXT` > `data/LICENSE.chromium`; write `data/SOURCE.txt` with both URLs, UTC date and `shasum -a 256` of each). `set -euo pipefail`, executable. Run it ONCE.
      Files: tools/keymap-gen/fetch.sh, tools/keymap-gen/data/{dom_code_data.inc,LICENSE.chromium,SOURCE.txt}
      Verify: script exits 0; `data/dom_code_data.inc` starts with Chromium's copyright header; SOURCE.txt lists the sha256 (design observed `841e2c58…e5b2`; a different hash is acceptable if item 3's invariants pass — record it in findings).

- [ ] 3. Implement keymap-gen (design §5.4): parser (strip `//` comments per line, tolerate `\r`, join, scan `DOM_CODE(`, split 7 top-level args, hex parse, string literal or NULL), filter (page 0x07 usage >= 0x04, page 0x0C, at least one native code), invariants (>= 200 parsed entries, no malformed field, no duplicate hid/win/mac among kept entries), deterministic emitter producing `KEYS`, `MAC_TO_HID: [u32; 128]`, `WIN_TO_HID: [u32; 512]` with the exact header/provenance lines, `--check` mode with the specified message and exit 1. Paths relative to `CARGO_MANIFEST_DIR/../..`. Unit tests on an inline fixture with a multi-line entry, a `//DOM_CODE(` commented entry, a NULL code, a page-0x01 entry (excluded) and a duplicate (error).
      Files: tools/keymap-gen/src/main.rs (split into src/parse.rs, src/emit.rs if > ~300 lines)
      Verify: `RUSTUP_TOOLCHAIN=stable cargo test -p keymap-gen` passes; clippy clean for the package.

- [ ] 4. Generate and check in the table; add core `types.rs` with the full `Hid` type (design §5.1: consts, `page`, `usage`, `modifier_bit`, derives incl. `Eq, Ord, Hash`) and `keymap/mod.rs` public API (`hid_to_mac, mac_to_hid, hid_to_win, win_to_hid, name`) with `#[rustfmt::skip] mod generated;`. Tests: tables sorted/unique, both round trips for every mapped entry, spot checks from design §5.4, plus round trips for all letters A–Z, digits 0–9, the 8 modifiers, 4 arrows, F1–F12, IntlBackslash (ISO), and E0-extended Windows keys (RightCtrl 0xE01D, RightAlt 0xE038, arrows, Insert/Delete/Home/End/PageUp/PageDown, NumpadDivide 0xE035, NumpadEnter, MetaLeft/Right, PrintScreen, NumLock) via `win_to_hid(sc & 0xFF, true)`.
      Files: crates/macwin-core/src/{lib.rs,types.rs,keymap/mod.rs,keymap/generated.rs}
      Depends on: 3.
      Verify: `RUSTUP_TOOLCHAIN=stable cargo run -q -p keymap-gen` then `… -- --check` exits 0; `RUSTUP_TOOLCHAIN=stable cargo test -p macwin-core keymap` passes; `cargo fmt --all --check` passes (generated file skipped).

- [ ] 5. Write `THIRD_PARTY_NOTICES` (B8) using `data/LICENSE.chromium` verbatim.
      Files: THIRD_PARTY_NOTICES
      Verify: file contains the Chromium copyright line and all three BSD clauses (compare against data/LICENSE.chromium with `diff` on the license block).

- [ ] 6. Write `deny.toml` (B8) and audit licenses locally: `RUSTUP_TOOLCHAIN=stable cargo metadata --format-version 1 --all-features | python3 -c '…'` printing every non-workspace package whose `license` expression has no OR-branch fully inside the allow-list (handle `AND`, `OR`, `/`, `WITH`). Adjust only by adding a permissive license that is genuinely needed; never allow GPL/AGPL/LGPL.
      Files: deny.toml
      Verify: the audit script prints no offending packages; `python3 -c "import tomllib;tomllib.load(open('deny.toml','rb'))"` parses it. (Real cargo-deny runs in CI only.)

### FEAT-002 — macwin-core domain modules (everything except the Engine)

- [ ] 7. Complete `types.rs` (all of design §5.1 + `Rect::new`/accessors/`GeometryError` from B3), `msg.rs` (design §5.2 + capability consts), `geometry.rs` + `layout.rs` (design §5.3: bounding box, clamp, t(), free-edge test, face(), entry point, virtual-cursor step returning `Moved/Exited`, normalization, leave mapping, `inset`, `EDGE_REENTRY_INSET`), with unit tests and the six proptest properties from §5.3 (random 1–4 screen grid layouts, both sides).
      Files: crates/macwin-core/src/{types.rs,msg.rs,geometry.rs,layout.rs,lib.rs}
      Depends on: 4.
      Verify: `RUSTUP_TOOLCHAIN=stable cargo test -p macwin-core` passes; clippy -D warnings clean for macwin-core.

- [ ] 8. Implement `remap.rs` (§5.5), `keys.rs` (§5.6 `KeySet`, `ButtonSet`, `release_all` ordering), `heartbeat.rs` (§5.7) and `safety.rs` (§5.8, Esc chord + NOTE about the brief's Q chord), each with the tests listed in the design (remap tables per direction, identity, involution proptest; release order + idempotence; heartbeat due/timeout boundaries with synthetic `Instant`s; chord side combos, partials, repeats, one fire per press, snapshot override).
      Files: crates/macwin-core/src/{remap.rs,keys.rs,heartbeat.rs,safety.rs,lib.rs}
      Depends on: 7.
      Verify: `RUSTUP_TOOLCHAIN=stable cargo test -p macwin-core` passes.

- [ ] 9. Implement `os/` helpers (§5.10): `INJECT_TAG`; `os::windows::{scan_to_hid, key_injection, WinKey, normalize_absolute, XBUTTON1/2}`; `os::macos::{flag consts, flags_changed, FlagsChange, flags_for, mask_from_flags, scroll_from_fixed, WheelAccumulator, ClickCounter}` — pure functions, compiled on every OS. Tests: fake LShift 0xE02A and AltGr fake LCtrl 0x21D dropped; Pause/NumLock VK overrides; `normalize_absolute` round trip for widths 1..=20000 under the floor model; flags_changed for all eight modifiers + CapsLock Tap + Fn None; wheel accumulator remainder carry; click counter interval/distance.
      Files: crates/macwin-core/src/os/{mod.rs,macos.rs,windows.rs}, crates/macwin-core/src/lib.rs
      Depends on: 8.
      Verify: `RUSTUP_TOOLCHAIN=stable cargo test -p macwin-core` passes; `RUSTUP_TOOLCHAIN=stable cargo clippy -p macwin-core --all-targets --target x86_64-pc-windows-gnu -- -D warnings` clean.

### FEAT-003 — macwin-core Engine (controller state machine)

- [ ] 10. Implement `engine/{mod.rs,session.rs}` exactly per design §5.9 (API, `Timings`, epochs/parity, LocalInput incl. `held_from_remote`, all transition-table rows, target injection rules, takeover accumulator, heartbeat reconciliation, lease renewal, constants, INFO transition logs without key identities). Tests: the 18 scenario tests from §5.9 asserting exact action lists, plus the property test over random interleavings of local events, peer messages, ticks, faults and disconnects checking invariants (a)–(e) and additionally "after Leave / on_disconnected / heartbeat timeout / shutdown the set of injected-but-not-released keys and buttons is empty" and "after any Controlling end the controller's forwarded downs minus ups is zero or covered by the peer's release".
      Files: crates/macwin-core/src/engine/{mod.rs,session.rs,tests.rs}, crates/macwin-core/src/lib.rs
      Depends on: 7–9.
      Verify: `RUSTUP_TOOLCHAIN=stable cargo test -p macwin-core engine` passes (proptest default 256 cases, completes in < 60 s); clippy clean.

### FEAT-004 — macwin-proto and macwin-platform

- [ ] 11. Implement macwin-proto per B2 (wire.rs, convert.rs, frame.rs, lib.rs API, tests/golden.rs, proptests).
      Files: crates/macwin-proto/src/{lib.rs,wire.rs,convert.rs,frame.rs}, crates/macwin-proto/tests/golden.rs
      Depends on: 7 (domain msgs), 10 is not required.
      Verify: `RUSTUP_TOOLCHAIN=stable cargo test -p macwin-proto` passes including the golden bytes `22 06 08 84 80 1c 10 01`.

- [ ] 12. Implement macwin-platform per B4 (traits, `PlatformError`, `UnsupportedPlatform`, `testing` fakes + their unit tests).
      Files: crates/macwin-platform/src/{lib.rs,testing.rs}, crates/macwin-platform/Cargo.toml (`[features] testing = []`)
      Depends on: 7.
      Verify: `RUSTUP_TOOLCHAIN=stable cargo test -p macwin-platform --all-features` passes; `cargo build -p macwin-platform` (no features) builds.

### FEAT-005 — macwin-net

- [ ] 13. Implement `fingerprint.rs`, `identity.rs` (incl. `config_dir`), `tls.rs` (pinned verifiers both directions, client/server quinn configs), `codes.rs`, `NetError`, with unit tests (fingerprint format/parse; config_dir branches via closures; verifier accepts the pinned cert and rejects another generated cert, for both verifier types).
      Files: crates/macwin-net/src/{lib.rs,fingerprint.rs,identity.rs,tls.rs,codes.rs}
      Depends on: 11.
      Verify: `RUSTUP_TOOLCHAIN=stable cargo test -p macwin-net --lib` passes.

- [ ] 14. Implement `link.rs`, `session.rs`, `endpoint.rs` per B5 and `tests/loopback.rs` (cases 1–8).
      Files: crates/macwin-net/src/{link.rs,session.rs,endpoint.rs,lib.rs}, crates/macwin-net/tests/loopback.rs
      Depends on: 13.
      Verify: `RUSTUP_TOOLCHAIN=stable cargo test -p macwin-net` passes; run it 3 times in a row to check for flakiness; clippy clean for the Windows target too.

### FEAT-006 — agent library, CLI, CI, README

- [ ] 15. Implement `driver.rs` per B6 (exported from `lib.rs`) and `tests/e2e_fake.rs` (cases 1–7).
      Files: crates/macwin-agent/src/{lib.rs,driver.rs}, crates/macwin-agent/tests/e2e_fake.rs
      Depends on: 10, 12, 14.
      Verify: `RUSTUP_TOOLCHAIN=stable cargo test -p macwin-agent --test e2e_fake` passes 3 runs in a row.

- [ ] 16. Implement `logging.rs`, `cli.rs`, `spike_net.rs` (+ `LatencyStats` unit tests) and `main.rs` per B7. Add `tests/spike_net.rs` (in-process dialer/listener on 127.0.0.1, count 50, both paths report n=50) and `tests/cli.rs` spawning `env!("CARGO_BIN_EXE_macwin")` with `MACWIN_CONFIG_DIR` set per child to tempdirs: `identity` twice -> same fingerprint, "created" only the first time, files 0600 (unix); `spike net --listen 127.0.0.1:0` + `--connect` with correct fps and `--count 20` -> dialer exit 0, stdout has `stream` and `datagram` lines with p50/p95/p99/max; wrong `--peer-fp` on the dialer -> non-zero exit within 10 s; kill children on test failure.
      Files: crates/macwin-agent/src/{main.rs,cli.rs,logging.rs,spike_net.rs,lib.rs}, crates/macwin-agent/tests/{spike_net.rs,cli.rs}
      Depends on: 14.
      Verify: `RUSTUP_TOOLCHAIN=stable cargo test -p macwin-agent` passes; manual smoke on loopback: two `cargo run -p macwin-agent -- spike net …` processes with MACWIN_CONFIG_DIR tempdirs print the latency report.

- [ ] 17. Write `.github/workflows/ci.yml` per B8.
      Files: .github/workflows/ci.yml
      Verify: `python3 -c "import yaml"` may be unavailable — instead verify with `ruby -ryaml -e 'YAML.load_file(".github/workflows/ci.yml")'` (system Ruby) that it parses, and confirm every `uses:` has an exact version tag; every `run:` command is one that passed locally in V-ALL.

- [ ] 18. Write `README.md` (English, concise): what macwin is (software KVM for one Mac + one Windows PC on a LAN; Phase 0a = foundation, no input capture yet), workspace layout (one line per crate + tools/keymap-gen), building on macOS (`RUSTUP_TOOLCHAIN=stable` note vs `rustup toolchain install 1.96.0 --profile minimal -c rustfmt,clippy -t x86_64-pc-windows-gnu`), cross-building for Windows (`brew install mingw-w64` prerequisite, `cargo build --release -p macwin-agent --target x86_64-pc-windows-gnu`), `macwin identity` and `macwin spike net` usage (fingerprint exchange, both modes, firewall note for non-loopback listen), security model (mTLS + SHA-256 pinning, no unencrypted mode), regenerating the keymap (`fetch.sh`, `cargo run -p keymap-gen`, `--check`), licensing pointer to THIRD_PARTY_NOTICES.
      Files: README.md
      Verify: every command in README was run (or is the documented mingw/rustup prerequisite) and matches the CLI's `--help`.

- [ ] 19. Run V-ALL (section A) and fix everything until clean. Record results in FEAT findings / final report, including: emergency-chord discrepancy, Chromium LICENSE fetch, cargo-deny on a Linux job, Windows backend runtime untested (no Windows machine), and the design.md truncation.
      Verify: all V-ALL commands succeed.
