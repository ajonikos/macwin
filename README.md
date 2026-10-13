# macwin

A software KVM for one Mac and one Windows PC on the same LAN: either
machine's keyboard and mouse can drive both screens, with a shared clipboard
and file copy-paste planned on top.

**Status: Phase 0 (spikes).** The core (protocol, controller state machine,
QUIC with mutual TLS pinning) is tested; real macOS and Windows backends for
capture, injection, screens and the clipboard exist and are exercised through
the `probe` and `spike` commands below. The end-to-end `spike kvm` shares one
machine's mouse and keyboard with the other, both ways. Clipboard sync, pairing
by code, a UI and installers come in later phases.

## Workspace

| Path | What it is |
|---|---|
| `crates/macwin-core` | Pure logic: vocabulary types, edge-mapping geometry, HID keymap, Ctrl/Cmd remap, pressed-key tracking, heartbeat, emergency chord, the controller state machine (`Engine`). No I/O, threads or clocks. |
| `crates/macwin-proto` | Strict protobuf codec (hand-written `prost` derives, no `protoc`), stream framing, version check. |
| `crates/macwin-net` | QUIC (quinn + rustls/ring) with mutual TLS and SHA-256 certificate pinning; identity storage; the `Link` abstraction. |
| `crates/macwin-platform` | Backend traits (capture, injection, screens) and in-memory fakes (`testing` feature). |
| `crates/macwin-platform-macos` | macOS backend: CGEventTap capture, CGEventPost injection, displays, NSPasteboard (empty on other OSes). |
| `crates/macwin-platform-windows` | Windows backend: low-level hooks, SendInput, per-monitor-v2 DPI, Win32 clipboard (empty on other OSes). |
| `scripts/macos` | Builds and signs `target/MacWin.app`, runs it through LaunchServices, stops it. |
| `crates/macwin-agent` | Session driver (library) and the `macwin` binary. |
| `tools/keymap-gen` | Generates the HID <-> macOS keycode <-> Windows scancode table from Chromium data. |

## Building

`rust-toolchain.toml` pins Rust 1.96.0. If your `stable` toolchain already is
1.96.0, you can run every command with `RUSTUP_TOOLCHAIN=stable` instead of
installing the pinned one:

```sh
rustup toolchain install 1.96.0 --profile minimal -c rustfmt,clippy -t x86_64-pc-windows-gnu
cargo test --workspace --all-features
cargo build --release -p macwin-agent          # target/release/macwin
```

### Windows binary from a Mac

```sh
brew install mingw-w64                          # provides x86_64-w64-mingw32-gcc
cargo build --release -p macwin-agent --target x86_64-pc-windows-gnu
# -> target/x86_64-pc-windows-gnu/release/macwin.exe
```

Windows CI builds natively with MSVC.

## Usage

Every machine has a self-signed identity, created on first use:

```sh
macwin identity
# fingerprint: 3f1c...e9 (64 hex digits)
# config dir: ~/Library/Application Support/macwin   (Windows: %APPDATA%\macwin)
```

Set `MACWIN_CONFIG_DIR` to use another directory.

### Latency spike

Exchange fingerprints first. Then run the listener on one machine and the
dialer on the other:

```sh
# on the PC (listener):
macwin spike net --listen 0.0.0.0:47800 --peer-fp <Mac fingerprint>
# on the Mac (dialer):
macwin spike net --connect <PC address>:47800 --peer-fp <PC fingerprint>
```

The dialer prints round-trip latency for the reliable input stream and for
datagrams:

```
stream   n=1000 lost=0 p50=...ms p95=...ms p99=...ms max=...ms
datagram n=1000 lost=0 p50=...ms p95=...ms p99=...ms max=...ms
```

Listening on a LAN address triggers the OS firewall prompt (Windows: allow on
private networks) and, on macOS 15+, the Local Network permission prompt.

### Probes (read-only)

```sh
macwin probe screens                 # monitors in native coordinates
macwin probe permissions [--prompt]  # macOS: Accessibility, Input Monitoring, Secure Input
macwin probe clipboard [--watch]     # format names and sizes, never the content
```

### Local input spikes

```sh
macwin spike capture [--seconds 10]  # print local input events, nothing swallowed
macwin spike capture --grab          # also swallow input and hide the cursor (ends after
                                     # --seconds or with the emergency chord)
macwin spike inject --demo           # cursor draws a square, then "hello" is typed
```

### KVM spike

On both machines, with each other's fingerprints (here the PC is to the right
of the Mac):

```sh
# PC
macwin.exe spike kvm --listen 0.0.0.0:47800 --peer-fp <Mac fingerprint> --peer-side left
# Mac (inside MacWin.app, see below)
scripts/macos/run.sh spike kvm --connect <PC address>:47800 --peer-fp <PC fingerprint> --peer-side right
```

Push the cursor over the shared edge to control the other machine; touch the
other machine's own mouse or keyboard to take control back. `--no-remap`
disables the Cmd <-> Ctrl swap. Ctrl+C in the terminal stops the session
(while that machine's keyboard is not being forwarded to the peer); the
emergency chord **Ctrl+Alt+Shift+Esc** always works.

### macOS app bundle

macOS grants input permissions per app and per code signature, so run macwin
from a signed bundle:

```sh
scripts/macos/bundle.sh                       # builds and signs target/MacWin.app
scripts/macos/run.sh probe permissions --prompt
scripts/macos/run.sh spike kvm ...            # output in ~/Library/Logs/macwin/macwin.log
scripts/macos/stop.sh
```

Grant **Accessibility** and **Input Monitoring** to MacWin in
System Settings > Privacy & Security, and allow **Local Network** when asked.
`bundle.sh` signs with `MACWIN_SIGN_IDENTITY` or the first "Apple Development"
identity, so the permissions survive rebuilds.

Known limits: windows of elevated (admin) Windows apps, the Windows lock
screen and UAC prompts do not accept remote input yet; macOS Secure Input
(password fields) hides keystrokes from macwin while active.

## Security model

- There is no unencrypted or unauthenticated mode. Every connection is
  TLS 1.3 over QUIC with client certificates.
- Each side accepts exactly one peer, identified by the SHA-256 fingerprint
  of its certificate (`--peer-fp`). A wrong or missing pin fails the
  handshake on both sides.
- Phase 0 stores the identity as PEM files (0600, inside a 0700 directory on
  Unix). Phase 1 moves it to Keychain / DPAPI and adds pairing by a short code.
- Emergency chord: **Ctrl+Alt+Shift+Esc** (Control+Option+Shift+Esc on a Mac).
  Capture backends check it on physical keys before anything is swallowed, and
  it always returns local control.

## Keymap data

`crates/macwin-core/src/keymap/generated.rs` is generated and checked in:

```sh
tools/keymap-gen/fetch.sh          # re-download Chromium's dom_code_data.inc (rarely needed)
cargo run -p keymap-gen            # regenerate
cargo run -p keymap-gen -- --check # CI: fails if the table is stale
```

## Licensing

Third-party data and its license are listed in `THIRD_PARTY_NOTICES`.
Dependencies must use permissive licenses; `deny.toml` (cargo-deny, run in CI)
enforces this. The project's own license is not decided yet.
