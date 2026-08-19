<!--
SPDX-FileCopyrightText: 2026 Time Bandits contributors
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Time Bandits

Screen-time management for Linux households — the piece missing next to Windows
Family Safety and Google Family Link.

Enforcement stands on systemd-logind and PAM, so quotas, time windows and
lockout work on **any** systemd desktop. The desktop-specific part is small and
sits on top: KDE Plasma is the first front end, and it is what adds
per-application reporting and a panel widget.

> **Status: early development.** The domain model is implemented and tested; the
> daemon, PAM module, plasmoid and hub are being built out. Not yet usable.

## What it does

- **Measures** which applications a child actually uses, with idle detection, so
  a session left open overnight does not burn the daily quota.
- **Enforces** daily and weekly quotas plus allowed time windows (bedtime), by
  locking the session *and* refusing the unlock and the next login.
- **Reports** to parents through a small household server with a web app, or
  works entirely offline on a single machine.
- **Stays honest about its limits.** Physical access defeats any user-space
  tool; see [docs/threat-model.md](docs/threat-model.md).

## Design in one picture

```
                     ┌──────────────────────────────┐
   parents ─PWA────▶ │  timebandits-hub  (Pi / NAS) │   optional
                     │  axum + SQLite + web app     │
                     └───────────────▲──────────────┘
                                     │ WebSocket, mTLS
┌────────────────────────────────────┴─────────────────────────────┐
│ child's PC                                                        │
│                                                                   │
│  root   timebanditsd ──── system D-Bus ──── tbctl                 │
│           ├── local SQLite (source of truth when offline)         │
│           ├── logind: lock / terminate session                    │
│           └── unix socket ◀── pam_timebandits.so                  │
│                                 in sddm, kde (lock screen), login │
│  ─────────────────────────────────────────────────────────────── │
│  child  timebandits-agent ── session D-Bus ── plasmoid            │
│           ▲ focus events from a KWin script                       │
│           └ idle via ext-idle-notify-v1, desktop notifications    │
└───────────────────────────────────────────────────────────────────┘
```

The split matters: everything running as the child is **untrusted**. The daemon
never depends on it to enforce anything. If the agent is killed or the KWin
script disabled, time keeps counting, the app is booked as `unknown`, and a
tamper event is recorded.

## What you install

| Package | Contents | Depends on a desktop? |
|---|---|---|
| `time-bandits` | Daemon, PAM module, session agent, `tbctl` | No — logind and PAM only |
| `time-bandits-plasma` | KWin focus script, plasmoid | KDE Plasma |
| `time-bandits-hub` | Household server and web app | No — runs headless |

A KDE household installs `time-bandits-plasma`, which pulls the core in. A
GNOME household can install `time-bandits` today and get quotas and lockout,
losing only the per-application breakdown until a GTK front end exists.

## Repository layout

| Path | Contents | Package |
|---|---|---|
| `crates/tb-core` | Domain model: quotas, schedules, decision engine, app identity | — |
| `crates/tb-proto` | Wire types shared by daemon, PAM module and hub | — |
| `crates/tb-daemon` | `timebanditsd` — the root service that measures and enforces | `time-bandits` |
| `crates/tb-pam` | `pam_timebandits.so` — refuses login and unlock | `time-bandits` |
| `crates/tb-agent` | `timebandits-agent` — idle and notifications | `time-bandits` |
| `crates/tb-cli` | `tbctl` — enrolment, status, policy, PAM setup, diagnostics | `time-bandits` |
| `kwin-script/`, `plasmoid/` | The only KDE-specific code in the project | `time-bandits-plasma` |
| `crates/tb-hub`, `web/` | Household server and its web app | `time-bandits-hub` |
| `packaging/` | Recipes for Arch, RPM and Debian; systemd units | — |

## Building

The toolchain is pinned with [mise](https://mise.jdx.dev):

```sh
mise install          # Rust, Node, pnpm, just
just check            # fmt, clippy, tests
```

## Licence

GPL-3.0-or-later. See [LICENSES/](LICENSES/).
