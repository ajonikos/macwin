# Roadmap after Phase 1 (user said "делай все": do every remaining phase, commit each)

Rules unchanged: in-chat work, RUSTUP_TOOLCHAIN=stable, never run capture/inject/clipboard/kvm
or launch MacWin.app on this Mac, tests on 127.0.0.1, pinned permissive deps, Russian replies.

## Phase 2: files and images through the clipboard, smooth scroll
- Clipboard content priority: files > text > image (Excel/Word text copies carry pictures).
- Images: PNG on the wire (ClipboardMsg::Image, max 64 MiB). mac: read public.png or
  public.tiff (-> PNG via NSBitmapImageRep), write PNG + TIFF. win: read "PNG" format or
  CF_DIBV5/CF_DIB (-> PNG via pure-Rust DIB parser + png crate), write "PNG" + CF_DIB.
  Echo signature = hash of decoded pixels (survives re-encoding by clipboard managers).
- Files: copied files are transferred eagerly on copy (limit config max_files_mb, default 512,
  0 = off) on a uni stream type 0x11: FilesHeader frame (clock + entries: '/'-relative path,
  size, dir, executable, mtime) + raw concatenated data. Receiver writes into a staging dir
  (cache dir/incoming/<id>), keeps only the latest transfer, then puts the top-level paths on
  its clipboard (mac: NSURL writeObjects; win: CF_HDROP + Preferred DropEffect=COPY) so
  Finder/Explorer paste copies them. Symlinks skipped. Names sanitized per OS.
  Echo signature = hash of manifest (rel paths + sizes), location independent.
- Capabilities clipboard.image / clipboard.files; sender only sends what the peer accepts.
- Smooth scroll: WheelUnit::Pixel end to end (mac trackpad continuous deltas).

## Phase 3: product shell
- LAN discovery by UDP broadcast (port 47801): find the pairing listener without typing an IP,
  and re-find the peer by fingerprint when its address changed (update config).
- Autostart: macOS LaunchAgent for MacWin.app `run`; Windows HKCU Run key. `macwin autostart`.
- Tray / menu bar UI (status, pause, clipboard toggle, pair, quit) — thin layer, logic tested.
- Packaging: macOS dmg via hdiutil; Windows zip + install.ps1 (copy, Run key, firewall rule);
  release profile strip; Windows GUI-subsystem binary for the tray.

## Phase 4: Windows secure desktop (lock screen / UAC) — evaluate feasibility; at least design.

## Status
- Phase 2 done (committed): images, files, smooth scroll. Image echo: the clipboard worker
  remembers the last image it read or wrote with a hash of its decoded pixels and reports a
  re-encoded copy with the earlier bytes (core keeps comparing bytes). Pixel wheel is gated by
  capability input.wheel-pixel; older peers get Notch120 (40 px = 1 notch).
- Phase 3 done (PR #2, stacked on PR #1: rustls 0.23.46 for RUSTSEC-2026-0346..0349, which had
  turned master's cargo-deny red): LAN discovery (pairing by code alone, a moved peer found
  again), autostart, tray (macOS menu bar, Windows notification area), single instance, dmg and
  Windows zip with install/uninstall scripts, CI package job with artifacts. One commit per
  part, as the user asked. Report: phase3-report.md.
- Phase 4: designed in phase4-design.md (feasible as an opt-in SYSTEM service with a session
  agent that follows the input desktop); implementing it needs Windows hardware.
- Next: hardware test of Phases 1-3 (unverified lists in the reports), then Phase 4 if wanted.
