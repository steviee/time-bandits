<!--
SPDX-FileCopyrightText: 2026 Time Bandits contributors
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Time Bandits

Screen-time management for KDE Plasma households — the piece Linux is missing
next to Windows Family Safety and Google Family Link.

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

## Repository layout

| Path | Contents |
|---|---|
| `crates/tb-core` | Domain model: quotas, schedules, decision engine, app identity |
| `crates/tb-proto` | Wire types shared by daemon, PAM module and hub |
| `crates/tb-daemon` | `timebanditsd` — the root service that measures and enforces |
| `crates/tb-agent` | `timebandits-agent` — per-session focus, idle and notifications |
| `crates/tb-pam` | `pam_timebandits.so` — refuses login and unlock |
| `crates/tb-hub` | `timebandits-hub` — household server and web app |
| `crates/tb-cli` | `tbctl` — enrolment, status, policy, PAM setup, diagnostics |
| `plasmoid/`, `kwin-script/`, `web/` | KDE and browser front ends |
| `packaging/` | RPM, systemd units, D-Bus and polkit policy, container images |

## Building

The toolchain is pinned with [mise](https://mise.jdx.dev):

```sh
mise install          # Rust, Node, pnpm, just
just check            # fmt, clippy, tests
```

## Licence

GPL-3.0-or-later. See [LICENSES/](LICENSES/).
