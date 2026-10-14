macwin for Windows
==================

Shares one keyboard, mouse and clipboard between this PC and a Mac on the same network.

Install
-------
1. Extract this folder.
2. Double-click install.cmd (or run: powershell -ExecutionPolicy Bypass -File install.ps1).
   It installs macwin for this user in %LOCALAPPDATA%\Programs\macwin, adds it to PATH,
   starts it at login, allows it through Windows Firewall on private networks (Windows asks
   for administrator rights once) and starts it. Its icon appears in the notification area.

Pair
----
On one computer choose "Pair with another computer..." in the macwin menu; it shows a code.
On the other choose "Enter pairing code..." (or run: macwin pair --code CODE).
Then tell macwin where the other screens are: "Other computer is on the left / right".

Notes
-----
- The network must be Private (Settings > Network & internet > Properties), or install with
  -AllNetworks to allow macwin on Public networks too.
- Windows SmartScreen may warn about the unsigned programs: More info > Run anyway.
- Emergency release: Ctrl+Alt+Shift+Esc.
- Log: %LOCALAPPDATA%\macwin\macwin.log

Uninstall: run uninstall.ps1 in the install folder (add -Purge to also delete the settings
and this PC's identity key).
