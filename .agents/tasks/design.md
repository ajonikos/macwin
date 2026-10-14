# macwin — Phase 0 ("spikes + skeleton") technical design

Status: v1 (first iteration; no design review existed). Scope: Phase 0 only. Product and technical decisions in the task statement are treated as fixed; this document turns them into an implementable design and flags every assumption it could not verify on this Mac.

## 0. Overview

Phase 0 creates a Cargo workspace that proves the hard parts of a symmetric macOS ↔ Windows software KVM end to end. `macwin-core` holds all OS-independent logic as pure, deterministic code (vocabulary types, edge-mapping geometry, generated HID keymap, modifier remap, pressed-key tracking, heartbeat timing, the emergency-chord detector and the controller state machine called the Engine). `macwin-proto` is a strict protobuf codec built from hand-written `prost` derives. `macwin-net` provides QUIC (quinn + rustls with the ring provider) with mandatory mutual TLS and SHA-256 certificate pinning, one control stream, one high-priority input stream and latest-wins datagrams. `macwin-platform` defines the backend traits; `macwin-platform-macos` (CGEventTap / CGEventPost / NSPasteboard) and `macwin-platform-windows` (low-level hooks / SendInput / clipboard listener, per-monitor-v2 DPI) are thin, cfg-gated FFI layers. The `macwin` agent CLI wires everything together; `macwin spike kvm` is the end-to-end demo and `macwin spike net` measures latency. Everything except the two OS backends' runtime behaviour is verifiable on this Mac: the Windows backend is compile-checked, clippy-checked and cross-built here and runtime-tested by the user.

One rule shapes the design: local input is swallowed only while a healthy Engine says so. The capture layer carries its own lock-free fail-safes (emergency chord, grab lease, bounded lock wait, panic guard) that restore local control even if the Engine, the network or the peer is broken.

## 1. Phase 0 deliverables

```
macwin/
  Cargo.toml  Cargo.lock  rust-toolchain.toml  .cargo/config.toml  deny.toml
  THIRD_PARTY_NOTICES  README.md  .gitignore (exists: target/, .agents/)
  .github/workflows/ci.yml
  crates/macwin-core/              # pure logic, forbid(unsafe)
    src/{lib.rs, types.rs, msg.rs, geometry.rs, layout.rs, remap.rs, keys.rs, heartbeat.rs,
         safety.rs, engine/{mod.rs, session.rs}, keymap/{mod.rs, generated.rs}, os/{mod.rs, macos.rs, windows.rs}}
  crates/macwin-proto/             # wire codec, forbid(unsafe)
    src/{lib.rs, wire.rs, convert.rs, frame.rs}  tests/golden.rs
  crates/macwin-net/               # QUIC + mTLS pinning, forbid(unsafe)
    src/{lib.rs, identity.rs, fingerprint.rs, tls.rs, endpoint.rs, session.rs, link.rs, codes.rs}  tests/loopback.rs
  crates/macwin-platform/          # traits + fakes (feature "testing"), forbid(unsafe)
    src/{lib.rs, testing.rs}
  crates/macwin-platform-macos/    # #![cfg(target_os = "macos")], FFI
    src/{lib.rs, capture.rs, inject.rs, screens.rs, clipboard.rs, permissions.rs, private_cursor.rs}
  crates/macwin-platform-windows/  # #![cfg(windows)], FFI
    src/{lib.rs, capture.rs, inject.rs, screens.rs, clipboard.rs, dpi.rs}
  crates/macwin-agent/             # [[bin]] name = "macwin"
    src/{main.rs, cli.rs, logging.rs, driver.rs, kvm.rs, spike_net.rs, spike_capture.rs, spike_clipboard.rs}
    tests/e2e_fake.rs
  tools/keymap-gen/                # package "keymap-gen", std-only generator
    src/main.rs  fetch.sh  data/dom_code_data.inc  data/SOURCE.txt
  scripts/macos/{bundle.sh, run.sh, stop.sh, Info.plist.in}
```

File names inside crates are guidance; crate boundaries, public APIs, constants and behaviour in this document are binding. No commits, no pushes.

## 2. Verified environment facts and the decisions they force

Checked during design (throwaway crates under /tmp, deleted afterwards; nothing captured input, touched the clipboard or prompted):

- **Toolchain pin vs. installed toolchain (genuine conflict).** rustc 1.96.0 (ac68faa20 2026-05-25) is installed as `stable-aarch64-apple-darwin`; the `x86_64-pc-windows-gnu` and `x86_64-apple-darwin` targets exist only on that toolchain. With `channel = "1.96.0"` rustup resolves `1.96.0-aarch64-apple-darwin`, which is not installed (verified: "override toolchain '1.96.0-aarch64-apple-darwin' is not installed"); rustup 1.29.1 would auto-install it (network download) and it would lack the Windows target. Decision: keep the pin exactly as requested (CI and fresh machines get 1.96.0). On this Mac every cargo command runs with `RUSTUP_TOOLCHAIN=stable` (the env var outranks the toolchain file; stable is exactly 1.96.0). No rustup state is changed. README documents both paths (`RUSTUP_TOOLCHAIN=stable` when stable is 1.96.0, or `rustup toolchain install 1.96.0 --profile minimal -c rustfmt,clippy -t x86_64-pc-windows-gnu`).
- **Windows cross-build works.** quinn 0.11.12 (runtime-tokio, rustls-ring) + rustls 0.23.45 (ring) + rcgen 0.14.10 (ring) + tokio 1.53.2 + windows-sys 0.61.2 built a release `.exe` for `x86_64-pc-windows-gnu` with only `linker = "x86_64-w64-mingw32-gcc"`. windows-sys 0.61 links through windows-link raw-dylib, which needs mingw's `dlltool` (present at /opt/homebrew/bin). The PE imports only system DLLs (KERNEL32, USER32, WS2_32, bcrypt, bcryptprimitives, ntdll, ADVAPI32, USERENV, UCRT `api-ms-win-crt-*`), no libgcc/libwinpthread. `aws-lc-rs` and `rustls-platform-verifier` are absent from the dependency graph.
- **macOS bindings** (compile-only check, never executed): objc2-core-graphics 0.3.2 exposes `CGEvent::tap_create`, `CGEvent::tap_enable` (the free fn `CGEventTapEnable` is deprecated), `CGEvent::post`, mouse/keyboard/scroll event constructors, integer/double field accessors (`EventSourceUserData`, `MouseEventDeltaX/Y`, `KeyboardEventAutorepeat`, `ScrollWheelEventFixedPtDeltaAxis1`, …), `CGEventSource::set_user_data`, `CGEventSourceSetLocalEventsSuppressionInterval`, `CGWarpMouseCursorPosition`, `CGAssociateMouseAndMouseCursorPosition`, `CGDisplayHideCursor/ShowCursor`, `CGGetActiveDisplayList`, `CGDisplayBounds`, `CGPreflight/Request{Listen,Post}EventAccess`; objc2-core-foundation exposes CFRunLoop/CFMachPort/CFRunLoopSource (v0 context with `perform`, `signal`, `wake_up`); objc2-app-kit exposes `NSPasteboard` and `NSEvent::doubleClickInterval`. The private symbols `_CGSDefaultConnection` and `CGSSetConnectionProperty` are exported by the SDK's CoreGraphics.tbd (MacOSX26.1.sdk), so `private-cursor-api` can link them directly.
- **Windows bindings** (compile-only check for the gnu target) with the feature list in §3. Paths that are easy to get wrong: `GlobalFree` is in `Win32::Foundation`, `MONITORINFOF_PRIMARY` in `Win32::UI::WindowsAndMessaging`, `CF_UNICODETEXT` in `Win32::System::Ole`.
- **prost**: `#[derive(prost::Message/Oneof/Enumeration)]` compiles under edition 2024 with `#![forbid(unsafe_code)]`; prost-derive generates `Debug` itself (never add `#[derive(Debug)]` to Message/Oneof types). `InputFrame{Key{hid: 0x070004, down: true}}` encodes to `22 06 08 84 80 1c 10 01`.
- **rustfmt**: `#[rustfmt::skip] mod generated;` makes `cargo fmt --all --check` skip the generated keymap file.
- **Chromium keycode data** (`…/dom_code_data.inc?format=TEXT`, base64): 612 lines, 246 `DOM_CODE(usb, evdev, xkb, win, mac, code, id)` entries (usage pages 0x00: 7, 0x01: 7, 0x07: 159, 0x0C: 73). Some entries span two lines; commented-out entries look like `//DOM_CODE(`; sentinels are win `0x0000` and mac `0xffff`; there are no duplicate win or mac codes; mac codes are ≤ 0x7e; win codes are `0x00XX` or `0xE0XX`. Observed sha256 `841e2c58e6388f14736717eb6e365c8b8e9767788e36339bf1d35a37ab48e5b2` (2026-10-12). Chromium's LICENSE is fetchable from `https://chromium.googlesource.com/chromium/src/+/main/LICENSE?format=TEXT` (BSD-3-Clause, "Copyright 2015 The Chromium Authors").
- Xcode 26.1.1 / MacOSX26.1 SDK on macOS 15.8.1: building with `MACOSX_DEPLOYMENT_TARGET=15.0` is fine.

## 3. Technology stack (locked once approved)

Rust 1.96.0, edition 2024, Cargo workspace (resolver 3). All versions exact-pinned in `[workspace.dependencies]`; crates use `dep = { workspace = true }`. `Cargo.lock` is kept.

| Crate | Pin | Features | Used by |
|---|---|---|---|
| prost | =0.14.4 | default (derive, std) | proto |
| bytes | =1.12.1 | default | proto, net |
| thiserror | =2.0.21 | default | all libraries |
| anyhow | =1.0.104 | default | agent, keymap-gen |
| tracing | =0.1.44 | default | core, net, platform-*, agent |
| tracing-subscriber | =0.3.23 | `default-features = false`, fmt, ansi, env-filter, std | agent |
| clap | =4.6.7 | derive | agent |
| tokio | =1.53.2 | rt-multi-thread, macros, net, time, sync, signal | net, agent |
| quinn | =0.11.12 | `default-features = false`, runtime-tokio, rustls-ring | net |
| rustls | =0.23.46 | `default-features = false`, ring, std | net |
| rcgen | =0.14.10 | `default-features = false`, crypto, ring, pem | net |
| ring | =0.17.14 | default | net (SHA-256) |
| libc | =0.2.190 | default | platform-macos (gethostname, thread QoS) |
| objc2 | =0.6.5 | default | platform-macos |
| objc2-core-foundation | =0.3.2 | `default-features = false`, std, CFBase, CFDate, CFMachPort, CFRunLoop, CFString | platform-macos |
| objc2-core-graphics | =0.3.2 | `default-features = false`, std, libc, CGDirectDisplay, CGError, CGEvent, CGEventSource, CGEventTypes, CGGeometry, CGRemoteOperation | platform-macos |
| objc2-app-kit | =0.3.2 | `default-features = false`, std, NSEvent, NSPasteboard | platform-macos |
| objc2-foundation | =0.3.2 | `default-features = false`, std, NSArray, NSProcessInfo, NSString | platform-macos |
| windows-sys | =0.61.2 | Win32_Foundation, Win32_Graphics_Gdi, Win32_System_DataExchange, Win32_System_LibraryLoader, Win32_System_Memory, Win32_System_Ole, Win32_System_Threading, Win32_UI_HiDpi, Win32_UI_Input_KeyboardAndMouse, Win32_UI_WindowsAndMessaging | platform-windows |
| proptest (dev) | =1.12.0 | `default-features = false`, std | core, proto |
| tempfile (dev) | =3.27.0 | default | net, agent |

Notes: `rustls-pki-types` is used through `rustls::pki_types` (including `pem::PemObject`), not as a direct dependency. No protoc, protox or prost-build (decision in §6.1). The crypto provider is always passed explicitly (`builder_with_provider`), never installed process-wide. All licenses in the resolved graph are MIT, Apache-2.0, ISC, BSD-3-Clause, Zlib, Unicode-3.0, or OR-expressions that include one of them (r-efi is `MIT OR Apache-2.0 OR LGPL-2.1-or-later`; memchr is `Unlicense OR MIT`), so the deny.toml allow-list in §14 passes without exceptions.

## 4. Workspace layout and crate graph

```
macwin-core            (no internal deps)                 #![forbid(unsafe_code)]
macwin-proto        -> core                               #![forbid(unsafe_code)]
macwin-net          -> core, proto                        #![forbid(unsafe_code)]
macwin-platform     -> core                               #![forbid(unsafe_code)]
macwin-platform-macos   -> platform, core                 #![cfg(target_os = "macos")], unsafe FFI allowed
macwin-platform-windows -> platform, core                 #![cfg(windows)], unsafe FFI allowed
macwin-agent (bin "macwin") -> core, proto, net, platform, platform-macos (cfg macos), platform-windows (cfg windows)
keymap-gen (bin)       std only (+ anyhow)
```

Core sits at the bottom and owns the domain vocabulary; proto depends on core and is purely a codec that converts and validates. This keeps prost types out of all logic and puts "parse, don't validate" at the wire boundary. The task listed proto first; the crate set is unchanged, only the dependency direction is a design choice.

Workspace rules:

- `[workspace.package]`: `version = "0.1.0"`, `edition = "2024"`, `rust-version = "1.96"`, `publish = false`. `license` is intentionally unset (the project's own license is an open user decision, §20).
- `[workspace.lints.rust]`: `unsafe_op_in_unsafe_fn = "deny"`, `unused_must_use = "deny"`. `[workspace.lints.clippy]`: `undocumented_unsafe_blocks = "warn"` (CI runs clippy with `-D warnings`, so every `unsafe` block needs a `// SAFETY:` comment). Every crate has `[lints] workspace = true`.
- `[profile.release]`: `lto = "thin"`, `strip = true` (Phase 3; before: `debug = "line-tables-only"`). `panic` stays `unwind`, because FFI callbacks rely on `catch_unwind` (§11).
- Platform crates start with `#![cfg(target_os = "macos")]` / `#![cfg(windows)]` and declare their OS dependencies under `[target.'cfg(...)'.dependencies]`, so `cargo build --workspace` compiles them as empty libraries on the other OS. The agent depends on them through target-specific dependencies. `macwin-platform` ships `UnsupportedPlatform` (every method returns `PlatformError::Unsupported`), so the agent also compiles on other hosts and exits with "unsupported OS".
- Features: `macwin-platform-macos/private-cursor-api` (off in the crate). `macwin-agent`: `default = ["private-cursor-api"]`, `private-cursor-api = ["macwin-platform-macos/private-cursor-api"]`. `macwin-platform/testing` exposes the fakes; the agent's dev-dependency enables it.
- Edition 2024 specifics: `extern` blocks must be written `unsafe extern "C" { … }`; `std::env::set_var` is `unsafe`, so no test may mutate the environment. Config-dir resolution therefore takes an env-lookup closure (§7.1).

`rust-toolchain.toml`:

```toml
[toolchain]
channel = "1.96.0"
components = ["rustfmt", "clippy"]
profile = "minimal"
```

`.cargo/config.toml`:

```toml
[target.x86_64-pc-windows-gnu]
linker = "x86_64-w64-mingw32-gcc"

[env]
MACOSX_DEPLOYMENT_TARGET = "15.0"
```

(`[env]` reaches rustc and build scripts such as ring's cc build; the linker is found on PATH, /opt/homebrew/bin here and on CI after `brew install mingw-w64`.)

## 5. macwin-core

Pure Rust with no I/O, no threads and no clocks: time is always passed in as `std::time::Instant`. Allowed dependencies: thiserror, tracing. Everything here is unit- and property-tested on the Mac.

### 5.1 Vocabulary types (`types.rs`)

```rust
pub struct Hid(pub u32);            // (usage_page << 16) | usage; page 0x07 keyboard, 0x0C consumer
impl Hid {
    pub const ESCAPE: Hid = Hid(0x07_0029);
    pub const LEFT_CTRL: Hid = Hid(0x07_00E0);  // E0 LCtrl, E1 LShift, E2 LAlt, E3 LGUI,
    pub const RIGHT_GUI: Hid = Hid(0x07_00E7);  // E4 RCtrl, E5 RShift, E6 RAlt, E7 RGUI
    pub fn page(self) -> u16;  pub fn usage(self) -> u16;
    pub fn modifier_bit(self) -> Option<u8>;    // E0..=E7 -> 0..=7
}
pub struct ModifierMask(pub u8);    // bit i <=> Hid(0x07_00E0 + i); same layout as the USB boot-report modifier byte
pub enum MouseButton { Left, Right, Middle, Back, Forward }
pub enum WheelUnit { Notch120, Pixel }  // Notch120: 120 = one detent (Windows WHEEL_DELTA). Pixel is reserved for Phase 2.
pub enum Os { MacOs, Windows }
pub enum Side { Left, Right }       // "peer is to my left/right"; fn opposite(self) -> Side
pub enum RoleKind { Idle, Controlling, Controlled }
pub struct Point { pub x: i32, pub y: i32 }
pub struct PointF { pub x: f64, pub y: f64 }
pub struct Rect { x: i32, y: i32, w: i32, h: i32 }   // half-open [x, x+w) × [y, y+h); Rect::new validates (§16)
pub struct ScreenInfo { pub id: u32, pub rect: Rect, pub native_per_logical: f64, pub primary: bool }

pub enum CapturedEvent {                     // produced by capture backends, consumed by the Engine
    MouseMove { pos: PointF, delta: PointF }, // native units; delta is post-acceleration
    Button { button: MouseButton, down: bool, pos: PointF },
    Wheel { dx: i32, dy: i32, unit: WheelUnit },
    Key { hid: Hid, down: bool, repeat: bool },
}
pub enum GrabCommand { Grab { exit: Point }, Release { warp_to: Point }, Rearm }
pub enum CaptureFault { EmergencyChord, LeaseExpired, SinkTimeout, TapDisabled, CallbackPanic }
pub enum InjectCommand {
    MoveTo(Point),
    Button { button: MouseButton, down: bool, at: Point },
    Wheel { dx: i32, dy: i32, unit: WheelUnit },
    Key { hid: Hid, down: bool, repeat: bool },
}
```

All derive `Debug, Clone, Copy (where possible), PartialEq` (plus `Eq, Ord, Hash` for `Hid`). Coordinates are always in the owning machine's native space: macOS = CG global display space in points (origin at the top-left of the main display, y down); Windows = virtual-screen physical pixels under per-monitor-v2 awareness (origin at the primary monitor's top-left; may be negative). Wheel convention on the wire and in these types: `dy > 0` scrolls toward the top of the content (wheel rotated away from the user), `dx > 0` scrolls right (the `WM_MOUSEWHEEL` / `WM_MOUSEHWHEEL` convention); each backend converts to and from its OS.

### 5.2 Domain messages (`msg.rs`)

```rust
pub enum ControlMsg  { Hello(Hello), Heartbeat(Heartbeat), Ping(Ping), Pong(Pong) }
pub enum InputMsg    { Enter(Enter), Leave(Leave), TakeControl(TakeControl), Key(KeyMsg), Button(ButtonMsg),
                       Wheel(WheelMsg), ModifierSync(ModifierMask), Ping(Ping), Pong(Pong) }
pub enum DatagramMsg { MouseAbs(MouseAbs), Ping(Ping), Pong(Pong) }

pub struct Hello { pub protocol_version: u32, pub capabilities: Vec<String>, pub os: Os, pub os_version: String,
                   pub device_name: String, pub screens: Vec<ScreenInfo>, pub peer_side: Option<Side>,
                   pub agent_version: String }
pub struct Heartbeat { pub epoch: u64, pub role: RoleKind }
pub struct Ping { pub id: u64, pub sent_micros: u64 }      // sender's monotonic clock; no clock sync needed
pub struct Pong { pub id: u64, pub ping_sent_micros: u64 }
pub struct Enter { pub epoch: u64, pub at: Point }          // `at` = virtual cursor start, target native coords
pub struct Leave { pub epoch: u64 }
pub struct TakeControl { pub epoch: u64 }
pub struct KeyMsg { pub hid: Hid, pub down: bool, pub repeat: bool }
pub struct ButtonMsg { pub button: MouseButton, pub down: bool, pub at: Point }
pub struct WheelMsg { pub dx: i32, pub dy: i32, pub unit: WheelUnit }
pub struct MouseAbs { pub epoch: u64, pub seq: u64, pub at: Point }

pub const CAP_INPUT_CAPTURE: &str = "input.capture";  // sender can control the receiver
pub const CAP_INPUT_INJECT: &str = "input.inject";    // sender can be controlled
pub const CAP_SPIKE_NET: &str = "spike.net";
```

Channel placement: Enter, Leave, TakeControl and ModifierSync travel on the input stream together with Key/Button/Wheel, because they must be totally ordered per direction (Enter → ModifierSync → keys → Leave). Putting Enter on the control stream would let a key overtake its Enter. The control stream carries session-level traffic (Hello, Heartbeat, Ping/Pong; Phase 1 clipboard). `MouseAbs` carries an `epoch` in addition to the specified `seq`: datagrams are unordered relative to the input stream, and a late datagram from an ended epoch must not move the cursor in a new one. `Ping`/`Pong` are accepted on every channel and answered on the channel they arrived on (used by `spike net`).

### 5.3 Geometry and edge mapping (`geometry.rs`, `layout.rs`)

Definitions for a validated, non-empty screen set S:

- `B(S)` is the bounding box (half-open) of all rects. A point is "on screen" when it lies inside some rect.
- `clamp(S, p)`: p if it is on screen; otherwise the nearest point (Euclidean) over each screen's closed pixel box `[x, x+w−1] × [y, y+h−1]` (ties go to the lowest screen index). Float points map to pixels with `floor`.
- `t(S, y) = clamp((y − B.top) / (B.bottom − B.top), 0, 1 − 1e−9)`: the proportional position along the edge, computed from the bounding box as specified.
- **Free edge.** A clamped point p inside screen s is at the free Right edge when `p.x ≥ s.right − 1` and `(s.right, floor(p.y))` is not on screen; at the free Left edge when `p.x ≤ s.left` and `(s.left − 1, floor(p.y))` is not on screen. Detecting the free edge of the screen under the cursor, rather than only the bounding-box edge, keeps crossing working for common laptop + external-monitor arrangements where the bounding-box edge is unreachable at some heights. This is the only multi-monitor accommodation in Phase 0; the mapping itself stays bounding-box proportional.
- **Facing screen** `face(S, y, D)`: among screens whose vertical span contains `floor(y)`, pick the minimal `left` for D = Left, or the maximal `right` for D = Right. If none spans y, take the screen with the smallest vertical distance (ties go to the lowest index) and clamp y into it.
- Constants: `EDGE_REENTRY_INSET = 2` native units.

Let P be my `peer_side` and F = P.opposite() the target's side that faces me.

**Enter** (Idle, local MouseMove at `pos`):

1. `p = clamp(local, pos)`. This handles both clamped and unclamped OS positions, which matters because whether Windows clamps `MSLLHOOKSTRUCT.pt` is unverified.
2. Require: p is at the free P edge, a session exists, the peer advertises `input.inject`, and no physical mouse button is held. Otherwise nothing happens.
3. `y_t = Bp.top + t(local, p.y) · (Bp.bottom − Bp.top)`; `s_f = face(peer, y_t, F)`. Entry point: x is `s_f.left` when F = Left, or `s_f.right − 1` when F = Right; y is `y_t` clamped into `s_f`.
4. `exit = floor(p)`.

**Virtual cursor motion** (Controlling): `v` is an f64 point in peer native space.

- Speed normalization: `d_t = d_s / npl_src * npl_tgt`. `npl_src` is the `native_per_logical` of the local screen containing `exit` (macOS: 1.0; Windows: DPI/96). `npl_tgt` is that of the peer screen containing `v`.
- `v' = v + d_t`. If `v'` is on screen, the result is `Moved(v')`.
- Otherwise let s be the peer screen containing v. If `v'` crosses s's F side (`v'.x < s.left` for F = Left, `v'.x ≥ s.right` for F = Right) and the neighbour pixel just past that side, at the height of v' clamped into s, is not on screen, the result is `Exited`. The exception: if any forwarded button is still down, the result is `Moved(clamp(peer, v'))` instead (no leaving mid-drag).
- Every other case is `Moved(clamp(peer, v'))`.
- After `Moved`, send `MouseAbs{at: floor(v)}`.

**Leave mapping**: `y_l = Bl.top + t(peer, v'.y clamped into Bp) · (Bl.bottom − Bl.top)`; `s_l = face(local, y_l, P)`. Warp target: x is `s_l.right − 1 − INSET` for P = Right, or `s_l.left + INSET` for P = Left, clamped into `s_l`; y is `y_l` clamped into `s_l`. This point is never on the free edge, so the next local event does not immediately re-enter.

`inset(exit)` is `exit` moved INSET units away from P and clamped into its screen. It is the warp target whenever a grab ends for any reason other than crossing back (TakeControl, fault, timeout, disconnect, shutdown, yielding to the peer).

Property tests (proptest, random 1–4 screen arrangements on a grid, either side):

- Entry points are always on a peer screen and on its free F edge.
- Leave points are on a local screen and never at the free P edge.
- The y mapping is monotonic.
- A single-screen round trip local → peer → local stays within 1 unit in y.
- `clamp` is idempotent and always returns an on-screen point.
- Normalization is the identity when `npl_src == npl_tgt`.

### 5.4 Keymap (`keymap/`) and `tools/keymap-gen`

`keymap/generated.rs` is checked in and declared as `#[rustfmt::skip] mod generated;`. It contains:

```rust
pub(crate) struct KeyEntry { pub hid: u32, pub win: u16 /* 0 = none; 0xE0xx = extended */, pub mac: u16 /* 0xFFFF = none */, pub code: &'static str /* "" if NULL */ }
pub(crate) static KEYS: [KeyEntry; N];        // sorted by hid, unique
pub(crate) static MAC_TO_HID: [u32; 128];     // 0 = none
pub(crate) static WIN_TO_HID: [u32; 512];     // index = (sc & 0xFF) | (0x100 if extended); 0 = none
```

Public API (`keymap/mod.rs`): `hid_to_mac(Hid) -> Option<u16>`, `mac_to_hid(u16) -> Option<Hid>`, `hid_to_win(Hid) -> Option<u16>`, `win_to_hid(scan: u16, extended: bool) -> Option<Hid>`, `name(Hid) -> Option<&'static str>`.

Kept entries are page 0x07 with usage ≥ 0x04, plus page 0x0C, each with at least one native code. Pages 0x00 and 0x01 (Hyper, Sleep, WakeUp, Fn, …) are excluded, so system-power keys are never forwarded in Phase 0. Chromium's win column is exactly "scan code | 0xE000 if extended" as reported by `KBDLLHOOKSTRUCT` (for example NumLock = 0xE045, Pause = 0x0045), which is the representation the Windows backend needs.

keymap-gen (std + anyhow, no network):

- `cargo run -p keymap-gen [-- --check]`. Default paths are relative to the workspace root (`CARGO_MANIFEST_DIR/../..`).
- Parsing: strip `//` comments line by line, join, scan for `DOM_CODE(`, split the seven arguments at top-level commas (entries may span lines), and parse hex. The code field is a string literal or `NULL`.
- It fails loudly if fewer than 200 entries parse, any field is malformed, or the kept entries contain a duplicate hid, win or mac code. Duplicates would need a deliberate tie-break rule; there are none today.
- Output is deterministic: header `// @generated by tools/keymap-gen from tools/keymap-gen/data/dom_code_data.inc. DO NOT EDIT.`, a provenance line ("Derived from Chromium ui/events/keycodes/dom/dom_code_data.inc, Copyright 2013 The Chromium Authors, BSD-3-Clause; see THIRD_PARTY_NOTICES"), then one entry per line.
- `--check` regenerates into memory, compares with the file, and exits 1 with "generated keymap is stale; run `cargo run -p keymap-gen`".
- Unit tests use a small fixture with a multi-line entry, a commented-out entry and a NULL code.
- `fetch.sh` (run once by the implementer, never by the build): curl `…/dom_code_data.inc?format=TEXT` | `base64 -D` into `data/dom_code_data.inc` (unmodified, so Chromium's header stays in place), then write `data/SOURCE.txt` with the URL, UTC date and `shasum -a 256`.

Core keymap tests:

- Table invariants: sorted and unique.
- Round trips `mac_to_hid(hid_to_mac(h)) == h` and `win_to_hid(hid_to_win(h)) == h` for every mapped entry.
- Spot checks: KeyA (win 0x1E, mac 0x00), ControlLeft (0x1D, 0x3B), MetaLeft (0xE05B, 0x37), NumpadEnter (0xE01C), PrintScreen (0xE037), Pause (0x0045), NumLock (0xE045), IntlBackslash (mac 0x0A), ArrowLeft (0xE04B, 0x7B).

### 5.5 Modifier remap (`remap.rs`)

`Remap::new(src: Os, dst: Os, enabled: bool)`. When `enabled && src != dst`, it swaps LeftCtrl (0xE0) ↔ LeftGUI (0xE3) and RightCtrl (0xE4) ↔ RightGUI (0xE7); everything else, including Alt/Option (0xE2/0xE6), is unchanged. Because the Mac's Cmd and the PC's Win key are both the HID GUI usage, this single swap gives exactly the specified tables:

- Mac keyboard to Windows target: Cmd → Ctrl, Ctrl → Win, Option → Alt.
- Windows keyboard to Mac target: Ctrl → Cmd, Win → Ctrl, Alt → Option.

Left and right are preserved. The controller (source side) applies it to every forwarded `KeyMsg` and to the `ModifierSync` mask; the target injects verbatim. `--no-remap` affects only the side it is passed to, as a controller. The emergency chord always uses physical, pre-remap codes. Tests: per-direction tables, identity when disabled or the OS is the same, and an involution proptest.

### 5.6 Pressed-state tracking (`keys.rs`)

`KeySet` wraps `BTreeSet<Hid>` (deterministic order). `ButtonSet` is a u8 mask. `release_all(at: Point) -> Vec<InjectCommand>` emits button ups at `at` first, then non-modifier key ups, then modifier key ups. Releasing modifiers last means a held Ctrl+C never degrades into a stray shortcut. The set is empty afterwards and calling it again is a no-op.

### 5.7 Heartbeat (`heartbeat.rs`)

`HeartbeatMonitor { interval: 250 ms, timeout: 1500 ms }` with `new(now)`, `due(now) -> bool`, `on_sent(now)`, `on_received(now)` and `timed_out(now) -> bool` (true when `now − last_rx ≥ timeout`). `on_received` is called for every valid frame on any channel. Heartbeats are generated by the Engine tick, so their arrival proves the peer's Engine is alive, not just its QUIC stack (QUIC ACKs would keep the transport alive even if the peer's logic were wedged).

### 5.8 Emergency chord detector (`safety.rs`)

`EmergencyDetector::on_key(&mut self, hid: Hid, down: bool, repeat: bool, snapshot: Option<ModifierMask>) -> bool`. It tracks the physical Ctrl/Alt/Shift state (either side) from key events. When `snapshot` is `Some` (macOS derives it from the event's flags), the snapshot replaces the tracked state, which fixes modifiers that were pressed before capture started. It fires on a non-repeat Escape key-down while at least one Ctrl, one Alt (Option) and one Shift are held: Ctrl+Alt+Shift+Esc, or Control+Option+Shift+Esc on a Mac keyboard. It is pure, allocation-free, and owned by the capture thread (§11). Tests: all side combinations fire; partial chords, repeats and an Esc pressed after releasing a modifier do not fire; one fire per press.

### 5.9 Engine — the controller state machine (`engine/`)

```rust
pub struct EngineConfig {
    pub local_os: Os, pub local_screens: Vec<ScreenInfo>, pub peer_side: Side, pub remap: bool,
    pub timings: Timings, // Default: heartbeat 250 ms / 1500 ms, lease 2000 ms, takeover 8.0 units per 500 ms
}
pub enum TransportRole { Dialer, Listener }   // decides epoch parity
pub struct PeerInfo { pub os: Os, pub screens: Vec<ScreenInfo>, pub capabilities: Vec<String>, pub device_name: String }
pub struct LocalOutcome { pub deliver: bool, pub actions: Vec<Action> }
pub enum Action {
    SendControl(ControlMsg), SendInput(InputMsg), SendDatagram(DatagramMsg),
    Inject(InjectCommand), Capture(GrabCommand), RenewLease(Instant), Close(CloseReason),
}
pub enum CloseReason { HeartbeatTimeout, ProtocolViolation(&'static str), Shutdown, Internal(&'static str) }

impl Engine {
    pub fn new(cfg: EngineConfig) -> Self;
    pub fn set_local_screens(&mut self, s: Vec<ScreenInfo>);           // only while no session
    pub fn on_connected(&mut self, now: Instant, peer: PeerInfo, role: TransportRole) -> Vec<Action>;
    pub fn on_disconnected(&mut self, now: Instant) -> Vec<Action>;
    pub fn on_local_event(&mut self, now: Instant, ev: CapturedEvent) -> LocalOutcome;
    pub fn on_control(&mut self, now: Instant, m: ControlMsg) -> Vec<Action>;
    pub fn on_input(&mut self, now: Instant, m: InputMsg) -> Vec<Action>;
    pub fn on_datagram(&mut self, now: Instant, m: DatagramMsg) -> Vec<Action>;
    pub fn on_tick(&mut self, now: Instant) -> Vec<Action>;              // driven every 50 ms
    pub fn on_capture_fault(&mut self, now: Instant, f: CaptureFault) -> Vec<Action>;
    pub fn shutdown(&mut self, now: Instant) -> Vec<Action>;
    pub fn role(&self) -> RoleKind;
}
```

State. `Engine` is long-lived:

- `LocalInput`: physical pressed keys, physical buttons held, last position, and `held_from_remote` (keys forwarded while Controlling and still physically held when the grab ended).
- `Option<Session>`: per connection. Holds `PeerInfo`, `Layout`, `Remap`, `parity`, `epoch_max`, `role`, the `HeartbeatMonitor`, the outgoing datagram `seq` and the target-side `last_seq`.
- `Role` is one of:
  - `Idle`.
  - `Controlling { epoch, v: PointF, exit: Point, npl_src, pre_grab: KeySet, fwd_buttons: ButtonSet }`.
  - `Controlled { epoch, injected_keys: KeySet, injected_buttons: ButtonSet, last_pos: Point, takeover_accum, last_phys_move: Instant }`.

Epochs. Each control period has an epoch number. Epochs created by the dialer are odd and those created by the listener are even; `next_epoch` is the smallest number greater than `epoch_max` with my parity. `epoch_max` is updated only from Enter messages sent or received on the ordered input stream, never from heartbeats. Parity makes simultaneous crossings deterministic: the higher epoch wins and the lower Enter is stale. An `Enter` with the wrong parity or epoch 0 is a protocol violation.

Local-event rules (physical events only; backends never pass our own or other synthetic input):

- Always update `LocalInput`. A Key down for an already-pressed key becomes `repeat = true`. This is how Windows repeats are detected; macOS sets the flag itself.
- In Idle or with no session: a key in `held_from_remote` is swallowed on repeat and on up (the up also removes it). Everything else is delivered.
- In Controlling, every event is swallowed except a pre-grab key up:
  - Key in `pre_grab`: an up is delivered locally **and** forwarded (remapped) as an up, and the key is removed from `pre_grab`; a repeat is swallowed and not forwarded.
  - Other keys: forward `Key{remap(hid), down, repeat}`.
  - Button: update `fwd_buttons` and forward `Button{at: floor(v)}`.
  - Wheel: forward.
  - MouseMove: apply §5.3.
- In Controlled: always deliver. A key down, button down or wheel event, or mouse movement accumulating ≥ 8 units of |dx|+|dy| within 500 ms (the accumulator resets after 500 ms without physical movement), triggers **take over**: `release_all`, `SendInput(TakeControl{epoch})`, role Idle.

Transitions (anything not listed is ignored, with a debug log):

| Current role | Trigger | Condition | Actions | Next role |
|---|---|---|---|---|
| Idle | local MouseMove | §5.3 Enter conditions | `Enter{e, entry}`, `ModifierSync{remap(mods ∩ pre_grab)}`, `Capture(Grab{exit})`, `RenewLease(now+lease)`; swallow | Controlling(e) |
| Idle | peer `Enter{e}` | e > epoch_max, peer parity | `Inject(MoveTo(entry clamped))` | Controlled(e) |
| Controlling(e) | local MouseMove | `Moved(v')` | `MouseAbs{e, ++seq, floor(v')}` | – |
| Controlling(e) | local MouseMove | `Exited` | `Leave{e}`, `Capture(Release{leave point})` | Idle |
| Controlling(e) | peer `TakeControl{e}` | – | `Capture(Release{inset(exit)})` | Idle |
| Controlling(e) | peer `Enter{e'}` | e' > e, peer parity | `Capture(Release{inset(exit)})`, `Inject(MoveTo)` | Controlled(e') |
| Controlling(e) | peer `Heartbeat{e, r}` | r ≠ Controlled | `Capture(Release{inset(exit)})` | Idle |
| Controlling(e) | capture fault | – | `Leave{e}`, `Capture(Rearm)` (the backend already released locally) | Idle |
| Controlled(e) | peer Key / Button / Wheel / ModifierSync | – | inject (rules below) | – |
| Controlled(e) | peer `MouseAbs{e, seq}` | seq > last_seq | `Inject(MoveTo(clamp))` | – |
| Controlled(e) | peer `Leave{e}` | – | `release_all` | Idle |
| Controlled(e) | peer `Enter{e'}` | e' > e, peer parity | `release_all`, `Inject(MoveTo)` | Controlled(e') |
| Controlled(e) | peer `Heartbeat{e, r}` | r ≠ Controlling | `release_all` | Idle |
| Controlled(e) | local physical input | takeover rule | `release_all`, `TakeControl{e}`; deliver | Idle |
| any | tick | heartbeat due | `Heartbeat{epoch_of_role_or_max, role}` | – |
| any | tick | `timed_out` | release/ungrab as on disconnect, `Close(HeartbeatTimeout)` | no session |
| Controlling | tick | session healthy | `RenewLease(now + 2 s)` | – |
| any | `on_disconnected` | – | Controlled: `release_all`; Controlling: `Release{inset(exit)}` | no session |
| any | `shutdown` | – | Controlling: `Leave{e}` + `Release{inset(exit)}`; Controlled: `release_all`; then `Close(Shutdown)` | no session |
| any | non-capture fault | – | `Capture(Rearm)` | – |

Whenever Controlling ends, `held_from_remote` = physically pressed keys − `pre_grab`.

Target injection rules:

- Key down (not repeat): insert into `injected_keys` and inject.
- Key repeat: inject only if the key is in the set.
- Key up: inject only if it was in the set (and remove it).
- `Button{at}`: `MoveTo(at)` then the button command, tracking `injected_buttons`.
- `ModifierSync{mask}`: release the injected modifiers not in the mask first, then press the missing ones.
- Positions from the peer are clamped to the local screens (defence in depth).

Heartbeat reconciliation is safe because of parity. A peer can only report my current epoch e with a role that is not the complement of mine after it has processed `Enter(e)` and then ended that epoch, so acting on it only shortens an epoch the peer already left. This also resolves the rare race where a Leave and an Enter for the winning epoch cross. Heartbeats with a different epoch are ignored.

Constants (`engine/mod.rs`): `TICK = 50 ms`, `HEARTBEAT_INTERVAL = 250 ms`, `HEARTBEAT_TIMEOUT = 1500 ms`, `GRAB_LEASE = 2000 ms`, `TAKEOVER_THRESHOLD = 8.0`, `TAKEOVER_WINDOW = 500 ms`, `EDGE_REENTRY_INSET = 2`.

Logging: role transitions at INFO ("entered peer screen, epoch 3", "peer took control", …). Event counts at TRACE. Key identities are never logged (§12.4).

Engine scenario tests. Each test feeds a scripted sequence and asserts the exact action list:

1. Enter at the right and left edges, with remapped ModifierSync.
2. No Enter without a session, without the peer's `input.inject`, or with a mouse button held.
3. Datagram seq increases; normalization from Windows at 1.5 to Mac at 1.0.
4. Leave on crossing back, then no immediate re-entry from the leave point.
5. No Leave while a forwarded button is held.
6. Remapped forwarding; repeat detection on Windows-style repeated downs.
7. Pre-grab key semantics; `held_from_remote` semantics.
8. Target inject, release on Leave, and the exact release order.
9. Takeover by key, and by movement above but not below the threshold.
10. TakeControl handling, including a stale epoch.
11. Simultaneous Enter (parity).
12. Stale and out-of-order MouseAbs.
13. Heartbeat cadence and timeout.
14. Reconciliation in both roles.
15. Fault handling.
16. Disconnect and shutdown in both roles.
17. Wrong-parity Enter leads to `Close(ProtocolViolation)`.
18. Lease renewal only while Controlling.

Property test: random interleavings of local events, peer messages, ticks and disconnects, checking that:

- (a) the target never emits non-release injections outside Controlled;
- (b) the controller never emits Key/Button/Wheel/MouseAbs outside Controlling;
- (c) after any epoch end, injected downs minus injected ups is zero per key and per button;
- (d) every `Grab` is followed by exactly one `Release` before the next `Grab` (with `Rearm` allowed after a fault);
- (e) epochs sent are strictly increasing with the right parity.

### 5.10 OS helper math (`os/`)

These are pure functions used by the backends and tested on the Mac.

- `INJECT_TAG: u64 = 0x6D61_6377_696E_0001` ("macwin" + 0001). It is written to `kCGEventSourceUserData` and to `dwExtraInfo`.
- `os::windows`:
  - `scan_to_hid(scan: u32, extended: bool)` returns None for `scan == 0` or unknown codes. That covers the fake LShift (0xE02A) and the AltGr fake LCtrl (0x21D), so both are dropped.
  - `key_injection(hid) -> WinKey::{Scancode{sc: u16, extended: bool}, Vk{vk: u16, extended: bool}}` with overrides Pause → `Vk{0x13, false}` and NumLock → `Vk{0x90, true}`; everything else uses the scancode.
  - `normalize_absolute(x, v_left, v_width) -> i32 = clamp(((x − v_left) * 65536 + v_width − 1) / v_width, 0, 65535)` (ceil). This is exact if Windows maps `pixel = floor(n * width / 65536)`; tests prove the round trip for widths 1..=20000 under that model.
  - XBUTTON constants (1 = Back, 2 = Forward).
- `os::macos`:
  - Device-dependent flag bits: LCtrl 0x1, LShift 0x2, RShift 0x4, LCmd 0x8, RCmd 0x10, LAlt 0x20, RAlt 0x40, RCtrl 0x2000. Device-independent bits: AlphaShift 0x10000, Shift 0x20000, Control 0x40000, Alternate 0x80000, Command 0x100000.
  - `flags_changed(keycode, flags) -> Option<FlagsChange>`. For the eight modifier keycodes (0x3B, 0x38, 0x3A, 0x37, 0x3E, 0x3C, 0x3D, 0x36), down is "that key's device-dependent bit is set". CapsLock (0x39) yields `Tap` (down then up), because macOS reports only lock-state changes. Fn (0x3F) yields None.
  - `flags_for(mask) -> u64` builds both bit families.
  - `ModifierMask` from flags.
  - `scroll_from_fixed(axis1, axis2) -> (dx, dy)`: `dy = round(axis1 · 120)`, `dx = round(−axis2 · 120)`. The axis2 sign is unverified (§20).
  - `WheelAccumulator`: 1/120 units to whole lines, carrying the remainder.
  - `ClickCounter::on_down(button, pos, now, interval) -> i64`: the count increments when the same button goes down within `interval` and within 4 points; otherwise it is 1.

<!-- PART3 -->
