<!--
SPDX-FileCopyrightText: 2026 Time Bandits contributors
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Trying it on a real machine

Everything in this project is covered by tests, and the tests prove the pieces.
They cannot prove the chain: that the lock screen shows our sentence, that SDDM
refuses the second login, that a bonus lands before the child gives up waiting.
That needs a real session and a person watching it.

This is the walkthrough for that. It takes about forty minutes.

## Before you start

**Use a throwaway VM.** The scripts refuse to run anywhere else unless you
override it deliberately, because they set an enforcing policy and lock a
session on purpose. This guard exists because an earlier test locked a
development machine for an afternoon.

A Fedora 44 KDE or Bazzite VM with 2 CPUs, 4 GB RAM and a graphical login is
enough. `quickemu`, `virt-install` or GNOME Boxes all work. You need a user
account you can log into graphically that is **not** the test user.

Build dependencies in the VM:

```sh
# Fedora / Bazzite
sudo dnf install git cargo rust make gcc cmake gettext \
                 pam-devel sqlite-devel qt6-qtbase-devel qt6-qtdeclarative-devel \
                 pamtester

# Arch
sudo pacman -S git rust make gcc cmake gettext pam sqlite qt6-base qt6-declarative
```

`pamtester` is optional but worth having: it lets several steps check
themselves instead of asking you.

## The escape hatch, before anything else

Read this now, not when you need it. If a login is ever refused when it should
not be:

1. `Ctrl+Alt+F3` for a text console, log in as root.
2. `sudo tests/vm/rescue.sh`

That pulls the emergency brake first (`/etc/timebandits/disable`, which stops
enforcement without needing the daemon to be reachable), then removes the PAM
lines, stops the daemon and deletes the test policies. The brake alone is
enough to get back in:

```sh
touch /etc/timebandits/disable
```

Keep a root TTY logged in on `Ctrl+Alt+F3` for the whole walkthrough. It costs
nothing and it is the difference between a two-second fix and a rescue disk.

## Running it

```sh
git clone https://github.com/steviee/time-bandits
cd time-bandits
sudo tests/vm/install.sh          # build, install, start the daemon
sudo tests/vm/verify.sh           # the walkthrough, step by step
```

Each step announces what it is about to do. Steps that need you at the screen
say so and wait. To repeat one:

```sh
sudo tests/vm/verify.sh 5         # just step 5
sudo tests/vm/verify.sh 6 7       # steps 6 and 7
```

## What each step proves

| # | Step | What it would mean if it failed |
|---|---|---|
| 1 | Preconditions | the install did not take |
| 2 | Test user in `kids` | nothing would ever be enforced — a policy alone is not permission |
| 3 | Measuring with nothing enforced | the KWin script or the agent is not reporting; the numbers behind every limit are wrong |
| 4 | PAM wired up, ordinary logins still work | the login stacks are broken — stop and run `rescue.sh` |
| 5 | Warning, then the session locks | the enforcement ladder does not fire |
| 6 | **The correct password is refused at the lock screen** | we are a screensaver, not a limit. This is the step that distinguishes this project from `timekpr` |
| 7 | Logging in again is refused (`plasmalogin` on Plasma 6.5+, `sddm` before) | a child restarts and carries on |
| 8 | A bonus works within seconds | a parent's decision does not reach the machine |
| 9 | Killing the agent does not stop the clock | the limit is bypassed by closing a program |
| 10 | The emergency brake releases everything | there is no way back if we get something wrong |

Steps 6 and 7 are the ones worth being fussy about. Read the message on the
screen: it should name the reason and say when time is available again, in the
language the machine is set to.

## When something does not work

```sh
tbctl doctor                       # the first thing to run, always
systemctl status timebanditsd
journalctl -u timebanditsd -f      # while reproducing
systemctl --user status timebandits-agent   # in the child's session
journalctl --user -t plasmashell -f         # widget and KWin script errors
tbctl status tbtest --json         # the numbers behind the report
```

`tbctl doctor` is written to answer the common failures directly, including the
most common one of all: an enforcing policy for a user who was never put in
`kids`, which records diligently and limits nothing.

Two things that look like faults and are not:

- **Time stops while the session is idle.** Two minutes of no input by default;
  `idle_threshold` in the policy.
- **The policy day starts at 04:00, not midnight.** Evening use belongs to the
  day it started on, which is what a household means by "today".

## On Bazzite, Kinoite and other image-based systems

`/usr` is read-only there, so `tests/vm/install.sh` will not work. Use the
system extension instead:

```sh
sudo packaging/sysext/build.sh --container   # stages in a Fedora container
sudo packaging/sysext/install.sh timebandits.raw
sudo systemd-sysusers                        # the groups; /etc is not in the image
sudo systemctl enable --now timebanditsd
sudo tbctl pam enable
```

`install.sh` schedules its own undo before it merges anything and cancels it
only once the machine has proved it can still authenticate. See
[packaging/sysext/README.md](../packaging/sysext/README.md) for why that guard
exists.

Then carry on with `tests/vm/verify.sh 2` onwards.

## Afterwards

```sh
sudo tests/vm/rescue.sh            # back to a normal machine
```

The test account is kept, in case you want to look at what was recorded.
`userdel -r tbtest` removes it.

Please report what you found — including the parts that worked, since a
walkthrough that has only ever been run by its author is not evidence of much.
