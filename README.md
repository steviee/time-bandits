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

---

## Status

**Early development. Not yet usable as a product.** What exists is solid and
tested; what is missing is missing entirely, and this table says which is which.

| Component | State |
|---|---|
| Domain model — quotas, schedules, decision engine | ✅ complete, 56 tests |
| `pam_timebandits.so` — refuses login and unlock | ✅ complete, 26 tests |
| `timebanditsd` — measures and enforces | ✅ works: tracking, logind locking, PAM socket |
| `tbctl` — manage policies and PAM setup | ✅ complete, 65 tests including end-to-end |
| `timebandits-agent` — focus, idle, notifications | ❌ not started |
| KWin script, plasmoid | ❌ not started |
| `timebandits-hub` — server and web app | ❌ not started |
| Packaging for Arch, RPM, Debian | ✅ all three build in CI |

---

## What it does

- **Measures** how long a child spends at the computer, and — once the session
  agent exists — in which applications, with idle detection so a session left
  open overnight does not burn the daily quota.
- **Enforces** daily and weekly quotas plus allowed time windows (bedtime), by
  locking the session *and* refusing both the unlock and the next login.
- **Reports** to parents through a household server with a web app, or works
  entirely offline on a single machine.
- **Says what it cannot do.** Physical access defeats any user-space tool. See
  [What it cannot do](#what-it-cannot-do).

---

## How it works

### Two levels of enforcement

Locking a session is not enough on its own: the child knows their own password
and can simply unlock again. So enforcement happens twice.

1. **systemd-logind** locks the session when time runs out, and can end it after
   a grace period.
2. **PAM** refuses the unlock and the next login. This is the half that makes
   the first one stick.

The PAM half works because lock screens authenticate through PAM. KScreenLocker
evaluates the `auth` stack of the `kde` service, which is exactly where the
module sits. An approach built on logind alone — which is what the existing
Linux tool in this space does — leaves that door open.

### The picture

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

### Everything running as the child is untrusted

The agent, the plasmoid and the KWin script all run as the child and can all be
killed. The daemon never depends on them to enforce anything. If they stop
reporting, time keeps counting, the application is booked as `unknown`, and a
tamper event is recorded. Losing them costs accuracy, never enforcement.

### The policy day

Quotas reset at 04:00 by default, not at midnight — otherwise a child playing at
23:50 is handed a fresh daily quota ten minutes later. The boundary is
configurable, and all arithmetic goes through a timezone-aware library, so days
that are 23 or 25 hours long behave correctly.

---

## How much of this is KDE-specific

Almost none of it:

| Concern | Mechanism | Portable? |
|---|---|---|
| Lock or end a session | `org.freedesktop.login1` | any systemd desktop |
| Refuse login and unlock | PAM | any distribution |
| Which apps are running | systemd scopes under `app.slice` | any systemd desktop |
| Is the user idle | `ext-idle-notify-v1` | any Wayland compositor supporting it |
| Warnings to the child | `org.freedesktop.Notifications` | any desktop |
| **Which window has focus** | **KWin script** | **KDE only** |

The last row is the entire desktop-specific surface — one file. It exists
because Wayland deliberately gives clients no way to observe each other's
windows, so every compositor needs its own answer. A GNOME front end is
therefore an added package, not a fork.

---

## What you install

| Package | Contents | Needs a desktop? |
|---|---|---|
| `time-bandits` | Daemon, PAM module, session agent, `tbctl` | no — logind and PAM only |
| `time-bandits-plasma` | KWin focus script, plasmoid | KDE Plasma |
| `time-bandits-hub` | Household server and web app | no — runs headless |

A KDE household installs `time-bandits-plasma`, which pulls the core in. A GNOME
household can install `time-bandits` and get quotas and lockout today, losing
only the per-application breakdown.

> Only `time-bandits` exists so far. The other two join the packaging in the
> same commit as the code they package.

---

## Installing

Packages are not published yet. To build them from a checkout:

```sh
make dist                    # source tarball + vendored dependencies

# Arch
cd packaging/arch && makepkg -si

# Fedora, RHEL, openSUSE
rpmbuild -ba packaging/rpm/time-bandits.spec

# Debian, Ubuntu
cp -r packaging/debian debian && dpkg-buildpackage -us -uc -b
```

Minimum Rust version is **1.90**, verified in CI rather than assumed. Debian
stable's compiler is older than that, so the `.deb` targets Debian unstable and
current Ubuntu.

See [packaging/README.md](packaging/README.md) for the file layout, why these
are upstream convenience packages rather than distribution-native ones, and the
per-distribution PAM module paths.

---

## Setting it up

```sh
# 1. Groups. Children go in kids, parents in parents.
groupadd -f kids
groupadd -f parents
usermod -aG kids   alice
usermod -aG parents dad

# 2. Start the daemon.
systemctl enable --now timebanditsd

# 3. Wire up PAM.  READ docs/pam-setup.md FIRST.
tbctl pam enable --dry-run     # shows exactly what would change
tbctl pam enable

# 4. Give a child some rules. Until this, nothing is limited.
tbctl policy set alice --enforcement true --daily 2h --daily sat=3h \
                       --window 'mon=15:00-19:00' --timezone Europe/Berlin

# 5. Check that it will actually do something.
tbctl doctor
```

**Step 3 is the one that can lock people out.** Read
[docs/pam-setup.md](docs/pam-setup.md) before touching `/etc/pam.d`, and never
test it on a machine you are logged into as the only user. `tbctl pam enable`
backs up every file before its first edit and `tbctl pam disable` restores
them, but a misplaced line in a PAM stack is its own kind of problem.

`tbctl doctor` exists because a half-configured setup is worse than an
unconfigured one: it looks like it is working. Every check it makes corresponds
to a way of being quietly ineffective — a missing lock-screen rule, a policy
still in observe-only mode, a socket nobody is listening on.

---

## Configuration

### The daemon

`/etc/timebandits/daemon.toml` covers how the daemon runs — not what any
particular child is allowed. Every option has a working default; the shipped
file is fully commented out and exists to document them.

| Option | Default | Meaning |
|---|---|---|
| `state_dir` | `/var/lib/timebandits` | where the usage database lives |
| `pam_socket` | `/run/timebandits/pam.sock` | socket the PAM module connects to |
| `tick_interval` | `5s` | how often usage is sampled |
| `hub_url` | unset | household server; unset means single machine |
| `managed_group` | `kids` | users treated as managed before a policy exists |

A typo in this file is an error, not a silent fallback to defaults. Running
unrestricted while an administrator believes their settings apply is how a
child ends up with no limits and nobody notices.

### Per-user rules

Rules live in the database, one policy per user:

| Field | Meaning |
|---|---|
| `enforcement` | `false` = observe only. **This is the default for new users.** |
| `daily_quota` | per weekday, e.g. `{ default = "2h", saturday = "3h" }` |
| `weekly_quota` | ceiling across Monday–Sunday |
| `allowed_windows` | per weekday, e.g. `15:00–19:00`. Empty means all day. |
| `day_start` | when the policy day begins; default `04:00` |
| `warnings` | remaining-time thresholds; default 15, 5 and 1 minutes |
| `grace_period` | time to save work between locking and ending a session |
| `idle_threshold` | how much inactivity stops the clock; default 2 minutes |
| `on_exhausted` | `lock`, `terminate`, or `lock_then_terminate` |
| `record_window_titles` | default `false` — see [Privacy](#privacy) |

`unlimited` has to be written out. It never arises from a missing field, so a
forgotten setting cannot silently grant unrestricted access.

Rules are managed with `tbctl`:

```sh
tbctl policy show alice
tbctl policy set alice --daily 90m --window 'sat=09:00-12:00'
tbctl policy set alice --enforcement false        # back to observing only
tbctl policy export alice --output alice.toml     # review, edit, re-import
tbctl grant-bonus alice 30m                       # takes effect within seconds
tbctl status                                      # everyone, at a glance
tbctl usage alice --week
```

Changes take effect within one tick. The daemon re-reads the policy and any
bonus grants on every pass, so a child locked out a moment ago is back in a few
seconds later without anything being restarted.

`tbctl` needs root, because it reads and writes the daemon's database directly.
The D-Bus interface that will let a parent do this without `sudo`, gated by
polkit, comes with the session agent.

---

## Privacy

Recording what a person does is only defensible if that person can see it.

- **Window titles are not recorded** unless a parent explicitly enables
  `record_window_titles`. The default is off.
- The plasmoid will show the child their own recorded usage, the active policy,
  and exactly what is being stored.
- Nothing leaves the household. There is no cloud service, no account, and no
  telemetry. The hub is a machine you own.

---

## What it cannot do

Stated plainly, because a parental-control tool that oversells itself is worse
than none:

- **Physical access defeats it.** A live USB stick, a rescue shell via
  `init=/bin/bash`, or moving the drive to another machine bypasses everything.
  A GRUB password and full-disk encryption help and are documented, but they are
  mitigation, not prevention.
- **An account with `sudo` or polkit admin rights defeats it.** Membership in
  `kids` and nothing else is a prerequisite.
- **Another operating system on the same machine** is outside its reach.
- **It does not filter content.** Time Bandits limits *time*, not what is seen.

The full analysis, including what happens when a child kills the agent or edits
`kwinrc`, is in [docs/threat-model.md](docs/threat-model.md).

---

## Known gaps

Honest list of what stands between here and something a household can use:

1. **No session agent**, so time is recorded but not attributed to
   applications — everything is booked as `unknown`. This is the next thing to
   build.
2. **Lock state comes from logind's `LockedHint`**, which the desktop sets
   voluntarily. A desktop that fails to clear it stops the clock, and the limit
   is never reached. The agent will observe lock state directly.
3. **`tbctl` needs root.** A parent has to use `sudo`. The polkit-gated D-Bus
   interface arrives with the agent.
4. **No hub, no web app**, so there is no parent-facing interface beyond the
   command line.
5. **No debug symbols** in the packages; see
   [packaging/README.md](packaging/README.md#debug-information).

---

## Documentation

| Document | Covers |
|---|---|
| [docs/architecture.md](docs/architecture.md) | Why these components exist, what each is authoritative for |
| [docs/pam-setup.md](docs/pam-setup.md) | PAM integration, safety properties, and how to get back in |
| [docs/threat-model.md](docs/threat-model.md) | Who this defends against, and what defeats it |
| [packaging/README.md](packaging/README.md) | Package structure, file layout, per-distribution details |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Ground rules, especially for anything touching PAM |

These are written to be split into a documentation site later; each file is a
page.

---

## Building from source

The toolchain is pinned with [mise](https://mise.jdx.dev):

```sh
mise install     # Rust, Node, pnpm, just, gh
just check       # what CI runs: fmt, clippy -D warnings, tests
just build       # release binaries
```

Tests that need a live system bus are ignored by default:

```sh
cargo test -p tb-daemon -- --ignored --nocapture
```

That one reads the real logind and prints every session it sees. It exists
because a mistyped D-Bus property name compiles perfectly and fails silently at
runtime — and it is what caught the daemon counting `systemd --user` as a second
person at the keyboard.

---

## Contributing

Contributions are welcome. Two rules matter more than the rest, and both are in
[CONTRIBUTING.md](CONTRIBUTING.md):

- **Never test enforcement on a machine you depend on.** Use a VM or a throwaway
  container.
- **Anything touching `crates/tb-pam` or `/etc/pam.d` needs a test.** A
  regression there does not produce a bug report; it produces a family that
  cannot log in.

---

## Roadmap

| | Milestone | State |
|---|---|---|
| M0 | Workspace, CI, licensing, docs | done |
| M1 | Tracking: daemon, local database | done |
| M2 | Enforcement: policy engine, logind, PAM module | done |
| M3 | `tbctl`: policies, PAM setup, diagnostics | done |
| M4 | Session agent, KWin script, plasmoid | **next** |
| M5 | Hub and parents' web app | |
| M6 | Enrolment, mTLS, offline sync | |
| M7 | Extra-time requests with push notifications | |
| M8 | Per-application limits and blocklists | |

---

## Licence

GPL-3.0-or-later, [REUSE](https://reuse.software)-compliant. No CLA —
contributions stay under the project licence. See [LICENSES/](LICENSES/).
