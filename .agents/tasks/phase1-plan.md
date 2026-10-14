# Phase 1 plan (in-chat, no workflows)

Scope: text clipboard sync, SPAKE2 pairing by code, identity key in Keychain/DPAPI,
config file + `macwin run` (config-driven session with reconnect).

Constraints: same as before (RUSTUP_TOOLCHAIN=stable, no capture/inject/clipboard/kvm
runs on this Mac, sockets on 127.0.0.1 in tests, pinned deps, permissive licenses).
Tests must never touch the real Keychain: MACWIN_KEY_STORE=file in CLI tests.

## Decisions
- Clipboard: eager sync on change (poll change count 250 ms on a dedicated thread),
  Lamport clock + tie-break (listener wins ties), echo guard by own-write change count,
  origin marker and text hash. Skip concealed/transient (mac: org.nspasteboard.*;
  win: ExcludeClipboardContentFromMonitorProcessing, Clipboard Viewer Ignore,
  CanUploadToCloudClipboard=0). No initial sync on connect. Capability "clipboard.text".
  Transport: one QUIC uni stream per update, type byte 0x10, priority -1, read_to_end
  with limit (wire max 16 MiB, config default 4096 KB). Deviation from design.md
  (control stream) because of the 64 KiB frame limit and head-of-line blocking.
- Pairing: separate ALPN "macwin-pair/1", TLS accepts any cert (pairing only),
  SPAKE2 (spake2 0.4.0, Ed25519) with 6-digit code, idA/idB = dialer/listener cert
  fingerprints, key confirmation HMAC over transcript (fps, both hello bytes, TLS
  exporter). Dialer confirms first, listener confirms, dialer sends Done.
  Listener burns the code after any attempt that reached the exchange, prints a new
  one, max 5 attempts, 10 min window.
- Keys: SecretStore trait in macwin-platform; Keychain (security-framework 3.7.0,
  service io.macwin.MacWin, account identity) / DPAPI file identity.dpapi / plain file
  identity.secret (MACWIN_KEY_STORE=file|keychain|dpapi). Secret = PEM bundle
  (cert + key). Migration from Phase 0 identity.{crt,key}.pem, verified, then deleted.
- Config: config.toml (serde + toml 1.1.8), deny_unknown_fields. listen, remap,
  [clipboard] enabled/max_text_kb, [peer] fingerprint/name/os/side/address.
  Dialer side of pairing stores the address; the other side listens.
- CLI: identity, pair [HOST[:PORT]] [--code] [--side] [--listen], unpair,
  config show | config set KEY VALUE, run. spike * unchanged.

## Steps
1. core msg + proto (ClipboardMsg, PairMsg, caps, version 0x0001_0001) + tests
2. core clipboard sync state machine + tests
3. platform: ClipboardBackend::snapshot, write_text -> u64, SecretStore + FileSecretStore,
   write_atomic, fakes; macOS/Windows impls; macOS device name
4. net: link clipboard uni streams, identity PEM bundle, pairing module + tests
5. agent: config, identity store, clipboard worker + driver, runner (reconnect),
   commands + CLI, tests
6. README, scripts, report, full sweep, license audit, commit

## Status
- Steps 1-4 done (proto msgs incl. ClipboardMsg::Other notice, core clipboard sync with
  proptest convergence, platform traits + fakes + Keychain/DPAPI/File stores + mac
  device_name, net link clipboard uni streams + identity PEM bundle + pairing with
  loopback tests). Identity::load_or_create removed from macwin-net (agent does it).
- Step 5 done: config.rs, identity_store.rs, clipboard.rs (worker thread), driver clipboard,
  runner.rs (reconnect, CONNECT_TIMEOUT 5 s), CLI pair/unpair/config/run, tests (cli pairing
  between processes, runner reconnect, e2e clipboard).
- BUG FIXED (from 0b): spawn_session dropped the capture backend on return -> real backends
  stop their thread on drop -> capture died at session start. Now the run task owns it
  (struct Capture). Also broke the Core -> control -> state -> sink -> Core cycle at end.
- Step 6 done: 223 tests, clippy clean both targets, macwin.exe 38 MB, licenses 152/0,
  MacWin.app rebuilt+signed (not launched), README, phase1-report.md. Committed 6df6e20.
- Waiting for the user's hardware test (phase1-report.md checklist) / go-ahead for the next phase.
