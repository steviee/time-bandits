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

## Enforcement

The tick loop is a state machine, not a set of conditions re-evaluated from
scratch. Locking is an **edge**, not a level: without that distinction a child
over their quota would receive a fresh lock request every few seconds for the
rest of the evening.

```
Running ──time is up──▶ Grace{since} ──grace expired──▶ Enforced
   ▲                         │                              │
   └──── bonus granted, new day, window opened ─────────────┘
```

`LockAction` decides which edges exist:

| Setting | Behaviour |
|---|---|
| `lock` | Lock immediately, stay locked. Never ends the session. |
| `terminate` | Do **not** lock; wait out the grace period, then end the session. Leaving the screen usable is the entire point — it is what makes saving open work possible. |
| `lock_then_terminate` | Lock immediately, then end the session after the grace period. |

Logging out resets the escalation, so the next login is handled as its own
event. While enforced, a session that somehow becomes unlocked is locked again;
PAM should make that impossible, but the loop does not rely on it.

### What counts as somebody using the computer

`logind` reports more sessions than there are people. On a live Plasma machine:

```
2  class=user     type=wayland      active=yes  → a person
3  class=manager  type=unspecified  active=yes  → systemd --user, not a person
```

Counting the `manager` session would double every minute; locking the display
manager's `greeter` session would make the machine unusable for the whole
household. Both are excluded, and a `live_logind_reports_usable_sessions` test
checks the mapping against the real bus — a mistyped D-Bus property name
compiles perfectly and fails silently at runtime.

### A known weak point

Whether the screen is locked comes from logind's `LockedHint`, which the desktop
sets voluntarily. If a desktop fails to clear it, time stops counting and the
limit is never reached. Until the session agent exists to observe lock state
directly, this is a dependency worth knowing about rather than one that is
solved.

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
