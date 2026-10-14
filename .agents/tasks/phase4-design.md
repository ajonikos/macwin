# Phase 4: Windows secure desktop (lock screen, UAC) — feasibility and design

Status: design only (the roadmap asks to evaluate feasibility and at least design). Nothing here
is implemented; it needs real Windows hardware to build and verify.

## Problem

Today macwin on Windows is a normal per-user process. Three things are out of its reach:

1. **The secure desktop.** The lock screen, the sign-in screen, Ctrl+Alt+Del and (by default)
   UAC prompts run on the `Winlogon` desktop of window station `WinSta0`. A process can only
   inject into and hook the desktop its threads are attached to, and a user process cannot
   open `Winlogon`. While that desktop is the input desktop, `SendInput` from macwin reaches
   nothing and its low-level hooks see nothing: a Mac that controls the PC is stuck at the
   lock screen, and the PC's own keyboard cannot control the Mac from there.
2. **Elevated windows** (UIPI): a medium-integrity process cannot send input to windows of
   elevated processes, and its hooks do not see input aimed at them.
3. **Before sign-in** nothing of macwin runs at all (the tray starts at login).

Ctrl+Alt+Del itself cannot be synthesized with `SendInput`; only `SendSAS` produces it.

## Facts that decide the design (checked against Microsoft documentation)

- `SendSAS`: the caller "must either be running as a service or have the uiAccess attribute …
  set to true"; a non-service caller must run as the current user or LocalSystem with UAC on;
  the policy *Computer Configuration > Administrative Templates > Windows Components > Windows
  Logon Options > Disable or enable software Secure Attention Sequence* must allow services
  (registry `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System\SoftwareSASGeneration`).
- uiAccess apps bypass UIPI (input into elevated windows) but only when Authenticode-signed
  (always enforced) and installed in `%ProgramFiles%`, `%ProgramFiles(x86)%` or
  `%SystemRoot%\system32` (default policy). With *Allow UIAccess applications to prompt for
  elevation without using the secure desktop*, UAC prompts move to the user's desktop (this is
  how Remote Assistance handles UAC). uiAccess does **not** reach the `Winlogon` desktop.
- A LocalSystem service can start a process in the console session with its own token
  duplicated and retargeted (`DuplicateTokenEx`, `SetTokenInformation(TokenSessionId)`, which
  needs `SeTcbPrivilege`, held by LocalSystem; `CreateProcessAsUserW` with
  `lpDesktop = "winsta0\default"`). A SYSTEM process in the session can `OpenInputDesktop` and
  `SetThreadDesktop` to follow the input desktop, including `Winlogon`. This is the approach of
  Synergy/Barrier/Input Leap (elevated daemon), RustDesk and other remote-control tools.

## Options

| | Lock / sign-in screen | UAC prompt | Elevated windows | Ctrl+Alt+Del | Cost |
|---|---|---|---|---|---|
| A. Status quo | no | no | no | no | none |
| B. Run the tray elevated | no | no | yes | no | an elevation per login; everything runs as admin |
| C. uiAccess tray | no | yes, with the policy above | yes | yes (uiAccess may call `SendSAS`) | signing certificate; per-machine install under Program Files |
| D. SYSTEM service + session agent | yes | yes | yes | yes (service calls `SendSAS`) | service, token and desktop handling, IPC, per-machine install, signing strongly advised |

Verdict: **feasible, choose D** (it is the only option that reaches the lock and sign-in
screens, the main complaint), keep the current per-user mode as the default and make D an
opt-in install ("secure desktop support"). C is a reasonable intermediate step if a signing
certificate is available before the service work is done.

## Design of D

### Processes

- **`macwin-svc.exe`** — a Windows service (LocalSystem, auto start, session 0). It owns no
  network and no input. It watches sessions (`RegisterServiceCtrlHandlerExW` with
  `SERVICE_ACCEPT_SESSIONCHANGE`: console connect/disconnect, logon, logoff) and keeps exactly
  one **session agent** running in the active console session (`WTSGetActiveConsoleSessionId`),
  restarting it with back-off when it exits. It calls `SendSAS(FALSE)` when the session agent
  asks for Ctrl+Alt+Del (the agent cannot: it is not a service).
- **Session agent** = `macwin.exe agent --system`, started by the service as SYSTEM in the
  console session. It is today's run loop (QUIC session, Engine, capture, injection) with two
  changes: its capture and injection threads follow the input desktop, and it holds the
  identity itself. Because the network endpoint lives in the SYSTEM process, no local process
  can drive input into the secure desktop through it: the only way in is the paired peer over
  mutual TLS with a pinned certificate.
- **Tray** (`macwin-tray.exe`, per user, as today) keeps the UI and the clipboard (clipboard
  data and received files belong to the user: the staging directory stays in the user's
  profile, files must not be written as SYSTEM). It talks to the session agent over a named
  pipe instead of running the run loop itself.

Before sign-in there is no tray: the session agent shares keyboard and mouse but no
clipboard, which is the expected behaviour at a sign-in screen.

### Following the input desktop

- A desktop watcher in the session agent polls `OpenInputDesktop` every 100–250 ms (there is
  no notification for desktop switches) and compares names via
  `GetUserObjectInformationW(UOI_NAME)` (`Default`, `Winlogon`, `Screen-saver`).
- On a switch, capture and injection move: a thread can only `SetThreadDesktop` while it owns
  no windows or hooks, so the capture backend tears down its hook thread (hooks, hider window)
  and starts a new one attached to the new desktop; the injector's thread calls
  `SetThreadDesktop` before its next `SendInput`. The Engine sees a capture `Rearm` (its fault
  path already handles a backend that restarts) and releases held keys before the switch, so
  nothing stays pressed across desktops.
- Screens are re-read after a switch (the sign-in screen may run at another DPI scaling);
  per-monitor-v2 awareness stays on.
- Ctrl+Alt+Del: no protocol change. The session agent recognises Ctrl+Alt+Delete among the
  keys it is about to inject (with the Ctrl/Cmd remap that is Cmd+Option+Forward Delete on a
  Mac keyboard), injects nothing for it and asks the service to call `SendSAS`.
- The emergency chord keeps working on every desktop: it is detected in the capture thread of
  whichever desktop is active.

### Identity and pairing

- The key moves from the user's DPAPI file to the session agent: DPAPI with
  `CRYPTPROTECT_LOCAL_MACHINE` in `%ProgramData%\macwin\identity.dpapi`, ACL: SYSTEM and
  Administrators only. On the switch to D the user's identity is migrated (re-encrypted)
  once by the installer, so the existing pairing survives.
- Pairing and config changes are requested by the tray over the pipe; the session agent runs
  them (it owns the listener ports). The config moves to `%ProgramData%\macwin\config.toml`.

### The pipe (tray <-> session agent)

- `\\.\pipe\macwin-<session id>`, created by the session agent with a DACL that grants only
  SYSTEM and the interactive user's SID of that session; the agent also checks the client's
  token (`GetNamedPipeClientProcessId` + `OpenProcessToken`) for that user and session.
- It carries: state events (the current `RunEvent`s), tray commands (pause, settings, pairing,
  quit sharing), and clipboard traffic (the tray's clipboard worker feeds the session's
  clipboard sharing). Nothing on it can inject input, so a malicious process of the same
  user gains nothing it does not already have (it could already read the clipboard).

### Security consequences

- Anyone who controls the paired Mac can now type at the sign-in screen and approve UAC
  consent prompts of an administrator account. That is inherent to every KVM with secure
  desktop support; it must be opt-in, explained in the installer, and visible in the tray
  ("secure desktop: on").
- The SYSTEM process is a larger attack surface: the QUIC and protobuf code runs as SYSTEM.
  Mitigations: the existing strict codec and pinning (nothing is parsed before the TLS
  handshake has pinned the peer, except QUIC itself and pairing), no local control channel
  that can inject input, and the service itself has no network code.
- Signing: an unsigned SYSTEM service that injects input looks like malware to SmartScreen
  and Defender heuristics; a code-signing certificate should be in place before release.

### Installation

- Per-machine installer (admin): binaries in `%ProgramFiles%\macwin`, `sc create` (or
  `New-Service`) for `macwin-svc`, the SAS policy value, firewall rules for the service's
  session agent, the tray in `HKLM\...\Run` or still per user. `uninstall.ps1` reverses it.
- The current per-user zip stays as the "no secure desktop" option.

### Work plan

1. Platform: desktop watcher, hook-thread restart per desktop, `SetThreadDesktop` in the
   injector, `SendSAS` binding; tested by a manual `spike secure-desktop` command (lock the
   PC with Win+L while a Mac controls it).
2. Service skeleton (`macwin-svc`: service main, session tracking, launching the session
   agent as SYSTEM, restart policy); CI can test install/start/stop on the runner.
3. Session agent mode and the pipe protocol (state, commands, clipboard), the tray as a pipe
   client; DPAPI machine-scope identity, migration.
4. Installer, docs, hardware test checklist (lock screen, sign-in after reboot, UAC prompt,
   Ctrl+Alt+Del, fast user switching, RDP session must be ignored).

Rough size: comparable to Phase 2. Main risks: capture on `Winlogon` (hooks there are less
documented; verify early with the spike), Defender reactions to an unsigned service, and
fast user switching (several sessions; only the console session is served).
