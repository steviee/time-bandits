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

## How much of this is KDE-specific

Almost none of it, which is not how the project started out and is worth being
explicit about:

| Concern | Mechanism | Portable? |
|---|---|---|
| Lock or end a session | `org.freedesktop.login1` | Any systemd desktop |
| Refuse login and unlock | PAM | Any distribution |
| Which apps are running | systemd scopes under `app.slice` | Any systemd desktop |
| Is the user idle | `ext-idle-notify-v1` | Any Wayland compositor supporting it |
| Warnings to the child | `org.freedesktop.Notifications` | Any desktop |
| **Which window has focus** | **KWin script** | **KDE only** |

The last row is the whole desktop-specific surface. It is one file, and it
exists because Wayland deliberately gives clients no way to observe each
other's windows — so each compositor needs its own answer. Losing it costs the
per-application breakdown and nothing else: quotas, time windows and lockout
carry on, with time recorded as `unknown`.

That is also exactly what happens when a child disables the script, which is
why the same code path serves both cases.

## Components

### `timebanditsd` (root, system service)

The only component that decides anything. It

- ticks every few seconds and asks `tb-core::evaluate` for a verdict,
- reads the rules from `/etc/timebandits/policy.d/<user>.toml`,
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

### Turning the focus reporter on

The script is a package like any other, and it does nothing until KWin is told
to load it:

```sh
kwriteconfig6 --file kwinrc --group Plugins --key org.timebandits.focusEnabled true
qdbus6 org.kde.KWin /Scripting org.kde.kwin.Scripting.start
```

`start()` will not reload a script that is already running — during development
`unloadScript org.timebandits.focus` has to come first, which is a good hour to
lose to a stale copy.

Its log line names the application and never the window title. The journal is
not a private place, and a caption is exactly what the privacy screen promises
is not collected unless a parent asks for it.

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

## Rules are files, usage is a database

Storage is split by what the data *is*, not by what is convenient:

| | where | why |
|---|---|---|
| rules | `/etc/timebandits/policy.d/<user>.toml` | configuration — read with `cat`, changed with an editor, kept in a backup as something a person can still understand years later |
| usage, events, bonus grants | `/var/lib/timebandits/state.db` | data — append-heavy, queried by time range, growing without bound |

There is no cache in front of the files. A policy is under a kilobyte and the
tick loop reads it every few seconds, which costs nothing and removes a class of
bug: an edit takes effect on the next tick, with no watcher, no reload command,
and no way for the daemon to act on a rule that is no longer written down.

Two consequences worth stating:

- **A file the daemon cannot parse is refused, not ignored.** Both outcomes mean
  no rules apply, but only one of them is quiet about it, and a typo that
  silently switches a child's limits off is exactly the failure this project
  exists to avoid. The daemon logs the file and the parse error.
- **A hand edit counts as current.** Somebody who opened the file and typed into
  it meant it, so a policy arriving from the hub has to carry a higher `version`
  to replace it.

Policies used to live as JSON blobs in the database — neither queryable as
structure nor readable as configuration. An installation from before the change
moves them into files on first start and leaves the old table in place.

## The policy day

Quotas reset at `day_start` (04:00 by default), not at midnight. A child playing
at 23:50 would otherwise receive a fresh daily quota ten minutes later. All
window and quota arithmetic goes through `jiff`, so days that are 23 or 25 hours
long behave correctly.
