# Phase 3 plan (product shell)

Scope (roadmap.md): LAN discovery, autostart, tray / menu bar UI, packaging.

Constraints: as before (pinned permissive deps, tests on 127.0.0.1, no capture/inject/
clipboard/kvm runs, Russian replies). This phase runs in a Linux sandbox: macOS and Windows
code is type-checked with cross clippy (`aarch64-apple-darwin` with throwaway SDK header
stubs outside the repo, `x86_64-pc-windows-gnu` with mingw); tests run on Linux (the CLI
tests need macOS/Windows) and in CI (macos-15, windows-2025). First fixed master's red
cargo-deny (rustls 0.23.46, PR #1); Phase 3 is stacked on it.

## Decisions
- Discovery: UDP 47801 (`discovery_port`), `DiscoveryMsg` in core + proto (magic `MWD1`,
  max 512 bytes). Query { nonce, seek }: `Pairing` (any machine waiting for pairing) or
  `Peer { target, sender }` with tags SHA-256(label, nonce, fingerprint), so fingerprints
  never travel in clear and only a machine knowing both answers. Answer { nonce, QUIC port,
  offer }: `Pairing { os, name, fingerprint }` or `Peer { proof }`. Answers are only hints:
  pairing is still authenticated by the code (SPAKE2), sessions by certificate pinning.
  Targets: 255.255.255.255, the /24 broadcast of the default-route IP, 127.0.0.1.
- `macwin pair` (waiting) also answers Pairing queries; `macwin pair --code C` without an
  address finds the waiting machine (exactly one, else list / explain).
- `macwin run`: the listening side answers Peer queries; the dialing side asks after a
  failed dial and, when the peer answers from a new address, dials it; on success the
  config address is updated if it was an IP (hostnames are kept).
- Config: `discovery = true`, `discovery_port = 47801` (keys `discovery`, `discovery-port`).
- Autostart: macOS LaunchAgent `~/Library/LaunchAgents/io.macwin.MacWin.plist` (RunAtLoad,
  `macwin tray`, Aqua sessions only, no KeepAlive); Windows HKCU Run value `macwin` =
  `"<dir>\macwin-tray.exe"`. `macwin autostart [on|off|status]`; the store is injectable
  (temp dir / test registry key in tests).
- Tray: OS layer is thin and polls the agent's view (version counter, 200 ms): icon state,
  tooltip, menu entries (label / separator / item with id, checked, enabled, optional text
  input), notices (alert on macOS, balloon on Windows). Clicks go back by id. The agent's
  controller (tokio task) owns the run loop: pause/resume, clipboard toggle, side,
  pair (waiting side, code shown), enter code (discovery + dial), start at login, quit.
  The emergency chord pauses instead of quitting. macOS: NSStatusItem + NSMenu + NSAlert,
  NSTimer in common modes; Windows: hidden window, Shell_NotifyIconW, TrackPopupMenu,
  in-memory dialog template for the code.
- `macwin tray`; `macwin` without arguments inside MacWin.app starts the tray;
  `macwin-tray.exe` is a GUI-subsystem binary. `run` and `tray` hold an instance lock
  (`File::try_lock` in the cache dir); the tray logs to a file.
- Packaging: release `strip = true`; `scripts/macos/dmg.sh` (hdiutil, app + Applications
  link); `scripts/windows/package.sh` (cross-build zip), `install.ps1` (per-user copy,
  autostart, firewall rule via one elevation), `uninstall.ps1`; CI job smoke-tests both.

## Steps
1. core/proto DiscoveryMsg + tests
2. net discovery (Responder, find_pairing, find_peer, broadcast_targets) + loopback tests
3. agent: config keys, pair via discovery, run re-find + config update, tests
4. autostart + CLI + tests
5. tray: platform types, agent model + controller (+ tests), macOS UI, Windows UI, entry
   points, instance lock, file log
6. packaging + CI package job
7. README, phase3-report.md, roadmap; full sweep; commit

## Status
- Step 0 done: rustls 0.23.46 (PR #1).
- Steps 1-7 done, one commit each (user: commit every important change): discovery messages,
  net discovery, agent discovery use (+ shared pairing module), autostart, tray controller,
  macOS menu bar, Windows notification area, entry points (tray command, macwin-tray.exe,
  instance lock, log file), packaging, CI package job, README, Phase 4 design, this report.
- Notes: binds retry while a just-closed QUIC endpoint holds the port (tray restarts);
  macOS notices are plain modal alerts (no abortModal: it may raise an ObjC exception).
- CI on PR #2 green: test macOS 300 (+1 ignored) / Windows 294, deny, package macOS/Windows.
