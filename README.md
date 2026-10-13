# macwin

A software KVM for one Mac and one Windows PC on the same LAN: either
machine's keyboard and mouse can drive both screens, and copied text,
images and files are shared between them.

**Status: Phase 2.** Pairing by a one-time code, the identity key in the
macOS Keychain / Windows DPAPI, a config file and `macwin run` (keyboard,
mouse with smooth trackpad scrolling, clipboard sharing of text, images and
files, automatic reconnects) are implemented and tested with fakes; the real
OS backends still need testing on hardware. A UI and installers come in
later phases.

## Workspace

| Path | What it is |
|---|---|
| `crates/macwin-core` | Pure logic: vocabulary types, edge-mapping geometry, HID keymap, Ctrl/Cmd remap, pressed-key tracking, heartbeat, emergency chord, the controller state machine (`Engine`), clipboard sync ordering. No I/O, threads or clocks. |
| `crates/macwin-proto` | Strict protobuf codec (hand-written `prost` derives, no `protoc`), stream framing, version check. |
| `crates/macwin-net` | QUIC (quinn + rustls/ring) with mutual TLS and SHA-256 certificate pinning; SPAKE2 pairing; identity bundles; the `Link` abstraction. |
| `crates/macwin-platform` | Backend traits (capture, injection, screens, clipboard, secret store) and in-memory fakes (`testing` feature). |
| `crates/macwin-platform-macos` | macOS backend: CGEventTap capture, CGEventPost injection, displays, NSPasteboard, login keychain (empty on other OSes). |
| `crates/macwin-platform-windows` | Windows backend: low-level hooks, SendInput, per-monitor-v2 DPI, Win32 clipboard, DPAPI (empty on other OSes). |
| `crates/macwin-agent` | Session driver, reconnecting runner, config (library) and the `macwin` binary. |
| `scripts/macos` | Builds and signs `target/MacWin.app`, runs it through LaunchServices, stops it. |
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

On a Mac, run every command through the signed app bundle
(`scripts/macos/run.sh <command>`, see below) so that macOS grants its
permissions and keychain access to MacWin.app.

### 1. Pair the machines (once)

On one machine (here the PC, whose screens are to the right of the Mac):

```sh
macwin.exe pair --side left
# listening on 0.0.0.0:47800
# pairing code: 123-456
# on the other machine run:
#   macwin pair 192.168.1.20:47800 --code 123456
```

On the other machine, run the printed command:

```sh
scripts/macos/run.sh pair 192.168.1.20:47800 --code 123456
# paired with "DESKTOP-1" (windows), fingerprint ...
# its screens are on the right of this machine
```

`--side` on either machine is enough; the other side is derived. Each code
works for one attempt: after a wrong code the waiting machine shows a new
one (at most 5 attempts, 10 minutes). The machine that dialed remembers the
address and dials it from then on; the other one listens.

### 2. Share

```sh
macwin.exe run                    # on the PC
scripts/macos/run.sh run          # on the Mac
```

Push the cursor over the shared edge to control the other machine; touch the
other machine's own mouse or keyboard to take control back. Trackpad and
Magic Mouse scrolling stays smooth on the PC. `run` reconnects by itself when
the network drops or the other machine restarts; Ctrl+C stops it, and so does
the emergency chord **Ctrl+Alt+Shift+Esc** (Control+Option+Shift+Esc on a Mac).

What you copy appears on the other machine's clipboard:

- **Text**, up to `clipboard-max-kb` (4 MiB by default).
- **Images** (screenshots, pictures copied in a browser or an editor), as
  PNG, up to 64 MiB.
- **Files and folders** copied in Finder or Explorer, up to
  `clipboard-files-max-mb` in total (512 MiB by default). They are sent
  right away when copied and wait in the cache directory
  (`~/Library/Caches/macwin/incoming`, Windows `%LOCALAPPDATA%\macwin\incoming`,
  or `MACWIN_CACHE_DIR`); paste in Finder or Explorer copies them out. Only
  the latest copy is kept there, across reconnects. A copy travels whole
  or not at all: an unreadable item, a name with a backslash or more than
  65 536 items keep it on this machine. Symlinks are skipped, and names
  that Windows does not accept are adjusted (`a:b.txt` becomes `a_b.txt`).

When a copy holds several kinds (Word and Excel put text and a picture),
files win over text, and text over an image.

### Configuration

`config.toml` lives in the config directory
(`~/Library/Application Support/macwin`, Windows `%APPDATA%\macwin`,
or `MACWIN_CONFIG_DIR`):

```sh
macwin config show
macwin config set side right               # where the peer's screens are
macwin config set address pc.local:47800   # dial this; `none` = listen
macwin config set listen 0.0.0.0:47800
macwin config set remap off                # keep Ctrl and Cmd/Win as they are
macwin config set clipboard off            # stop sharing the clipboard
macwin config set clipboard-max-kb 4096    # largest shared text
macwin config set clipboard-images off     # text and files only
macwin config set clipboard-files off      # text and images only
macwin config set clipboard-files-max-mb 2048  # largest shared copy of files
macwin unpair                              # forget the peer
```

### Identity

Every machine has a self-signed identity, created on first use:

```sh
macwin identity
# fingerprint: 3f1c...e9 (64 hex digits)
# key: login keychain, item "io.macwin.MacWin" / "identity"
# config dir: ~/Library/Application Support/macwin
```

The certificate and key are kept in the login keychain on macOS and in a
DPAPI-encrypted `identity.dpapi` on Windows. `MACWIN_KEY_STORE=file` keeps
them in an unencrypted `identity.secret` (mode 0600) instead, for headless
machines and tests. Phase 0 identity files are moved into the store
automatically.

### Diagnostics and spikes

```sh
macwin probe screens                 # monitors in native coordinates
macwin probe permissions [--prompt]  # macOS: Accessibility, Input Monitoring, Secure Input
macwin probe clipboard [--watch]     # format names and sizes, never the content
macwin spike capture [--grab]        # print (and with --grab, swallow) local input
macwin spike inject --demo           # cursor draws a square, then "hello" is typed
macwin spike net --listen 0.0.0.0:47800 --peer-fp <hex>    # latency, with manual pins
macwin spike kvm --connect <host>:47800 --peer-fp <hex> --peer-side right
```

### macOS app bundle

macOS grants input permissions per app and per code signature, so run macwin
from a signed bundle:

```sh
scripts/macos/bundle.sh                       # builds and signs target/MacWin.app
scripts/macos/run.sh probe permissions --prompt
scripts/macos/run.sh run                      # output in ~/Library/Logs/macwin/macwin.log
scripts/macos/stop.sh
```

Grant **Accessibility** and **Input Monitoring** to MacWin in
System Settings > Privacy & Security, and allow **Local Network** when asked.
On macOS versions that ask before apps read the clipboard, allow MacWin
under **Paste from Other Apps**. `bundle.sh` signs with
`MACWIN_SIGN_IDENTITY` or the first "Apple Development" identity, so the
permissions and keychain access survive rebuilds.

Known limits: windows of elevated (admin) Windows apps, the Windows lock
screen and UAC prompts do not accept remote input yet; macOS Secure Input
(password fields) hides keystrokes from macwin while active. Copied files
travel when they are copied, not when they are pasted, so a large copy
takes a moment to become pasteable on the other machine.

## Security model

- There is no unencrypted or unauthenticated session. Every connection is
  TLS 1.3 over QUIC with client certificates, and each side accepts exactly
  one peer, identified by the SHA-256 fingerprint of its certificate.
- Pairing (separate ALPN `macwin-pair/1`) runs SPAKE2 with the one-time code
  over a TLS connection that accepts any certificate. Both certificates'
  fingerprints and a TLS exporter value are bound into the SPAKE2 identities
  and the key confirmation, so a man in the middle needs the code, and each
  attempt tests at most one guess.
- The identity key lives in the Keychain / DPAPI (see above).
- Clipboard: each kind (text, images, files) is shared only while both
  machines enable it. Content marked private by password managers
  (`org.nspasteboard.Concealed` / `Transient`, Windows
  `ExcludeClipboardContentFromMonitorProcessing`,
  `CanUploadToCloudClipboard = 0`) is never sent, and clipboard content and
  file names are never logged. Received file paths are checked (no `..`,
  absolute paths or drive letters) before anything is written, and files
  are only ever written inside the cache directory.
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
