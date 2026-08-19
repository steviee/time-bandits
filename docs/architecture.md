<!--
SPDX-FileCopyrightText: 2026 Time Bandits contributors
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Architecture

## Why these components

Wayland has no general way for one application to observe another's windows.
That single fact shapes the whole client design.

| Need | Only workable mechanism | Consequence |
|---|---|---|
| Which app has focus? | A KWin script (`workspace.windowActivated`, `callDBus`) | Runs inside the compositor, i.e. as the child |
| Which apps are running? | systemd scopes under `app.slice` (Plasma ≥ 5.19) | Readable by root, independent of the session |
| Is the user idle? | `ext-idle-notify-v1`, exposed via the session | Again inside the session |
| Lock or end a session | `org.freedesktop.login1` | Root, no session needed |
| Refuse login / unlock | PAM | Root, no session needed |

The observation paths run in the child's session and are therefore killable.
The enforcement paths do not. The daemon treats the first group as *hints* and
the second as *authority*.

## Components

### `timebanditsd` (root, system service)

The only component that decides anything. It

- ticks every few seconds and asks `tb-core::evaluate` for a verdict,
- keeps usage in a local SQLite database, which stays authoritative when the hub
  is unreachable,
- answers the PAM module over a Unix socket at `/run/timebandits/pam.sock`,
- exposes `org.timebandits.Daemon1` on the system bus for `tbctl` and the agent,
- talks to the hub over a WebSocket when one is configured.

Ticks are driven by a monotonic clock, so changing the wall clock cannot buy
extra time. Wall-clock jumps are detected and logged.

### `timebandits-agent` (per user, session service)

Reports what only the session can see — focused application, idle time, screen
lock state — and renders warnings as desktop notifications. It holds no
authority: a missing agent degrades accuracy, never enforcement.

### `pam_timebandits.so`

Placed in the `account` stack of `sddm`, `login` and `sshd`, and in the **`auth`**
stack of `kde` — KScreenLocker only evaluates `auth`, which is exactly why a
child cannot simply unlock with their own password when time has run out.

The module deliberately avoids D-Bus and async runtimes. It opens a Unix socket,
writes one line of JSON, reads one line back, and gives up after 300 ms. It
returns `PAM_IGNORE` for `root` and for members of `parents` before doing
anything else.

### `timebandits-hub` (optional)

A single binary (or container) that stores policies and usage for the household
and serves the parents' web app. Clients enrol once with a one-time code and
authenticate with a device certificate afterwards.

## Time accounting

1. The agent reports focus and idle; the daemon independently resolves the
   application from the process's systemd scope.
2. `AppObservation::best_of` keeps the more trustworthy of the two.
3. `SegmentBuilder` collapses consecutive ticks into `UsageSegment`s. Suspends
   and clock jumps end a segment at the last tick actually observed, so time the
   machine spent asleep is never credited.
4. Segments carry a UUID, which makes syncing to the hub idempotent.

## The policy day

Quotas reset at `day_start` (04:00 by default), not at midnight. A child playing
at 23:50 would otherwise receive a fresh daily quota ten minutes later. All
window and quota arithmetic goes through `jiff`, so days that are 23 or 25 hours
long behave correctly.
