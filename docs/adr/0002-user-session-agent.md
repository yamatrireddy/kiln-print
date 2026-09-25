# ADR 0002 — The agent runs in the user's session, not as a system service

- **Status:** Accepted
- **Date:** 2026-09-25

## Context

The agent must "run in the background and optionally start automatically with the operating system." On Windows the obvious choice is a Windows Service, but services run in session 0 under a service account.

## Decision

The agent is a **per-user background process started at logon**. On Windows it is registered under `HKCU\...\Run` (or a logon-triggered scheduled task) by the installer, with a tray icon. On macOS it is a LaunchAgent, not a LaunchDaemon. On Linux it is a systemd `--user` unit or XDG autostart entry.

## Rationale

1. **GDI printing from services is unsupported.** Microsoft documents printing from a Windows service as unsupported for the GDI print path, and many drivers break or hang in session 0. Phase 1 text printing uses GDI, and Phase 2 PDF, image and HTML printing will too.
2. **Printer visibility is per user.** Network printer connections (`\\server\printer`), per-user default printers and many USB driver queues exist only in the user's profile. A LocalSystem service cannot see them.
3. **Consent needs the user's desktop.** The trust prompt ("Application X wants permission to use your printers") must appear on the interactive desktop. A service cannot show UI and would need an extra broker process anyway.
4. **Least privilege.** The agent never needs administrator rights to print. Running as the user means a compromise of the agent gains nothing beyond what the user already has.

## Consequences

- There is one agent per logged-in user. On multi-user terminal servers each session gets its own agent, so the port must be per-session. This is a Phase 6 item: pick a free port per session and publish it through a per-user discovery file.
- Kiosk and headless machines use auto-logon, or a Phase 6 "service mode" limited to RAW, TCP and serial printing (no GDI). That mode is explicitly out of scope until then.
- Data (database, admin token, logs) lives in per-user local AppData, protected by the user profile's ACL.
