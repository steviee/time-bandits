<!--
SPDX-FileCopyrightText: 2026 Time Bandits contributors
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Threat model

## Who we defend against

A curious child with a normal user account on a machine they use every day, no
`sudo`, and plenty of time and motivation. Not a remote attacker, and not
someone with physical access to the disk.

## What is in scope

| Attempt | Response |
|---|---|
| Kill `timebandits-agent` | Time keeps counting, app booked as `unknown`, tamper event recorded |
| Disable the KWin script in `~/.config/kwinrc` | Same; the systemd-scope sensor still resolves applications |
| Unlock the lock screen with their own password | PAM `auth` stack for the `kde` service refuses |
| Log in again after a lockout | PAM `account` stack for `sddm` / `login` / `sshd` refuses |
| Stop the daemon | Not permitted without privileges; if it is unreachable the PAM module fails **closed** for members of `kids` |
| Change the system clock | `timedatectl` is gated by polkit and denied for `kids`; ticks use a monotonic clock regardless |
| Unplug the network to escape the hub | The daemon holds the last known policy and buffers usage locally; enforcement continues |

## What is explicitly out of scope

- **Physical access.** A live USB stick, a rescue shell via `init=/bin/bash`, or
  pulling the drive defeats any user-space tool. Optional hardening (a GRUB
  password, full-disk encryption) is documented in the admin guide, but it is
  mitigation, not prevention.
- **An account with `sudo` or polkit admin rights.** A child in `wheel` can
  disable everything. Membership in `kids` and nothing else is a prerequisite.
- **Another operating system on the same machine.**
- **Content filtering.** Time Bandits limits *time*, not what is seen.

## Enforcement needs two independent things

A policy is not permission. The daemon additionally requires the user to be in
the managed group before it will lock anything, because the two are set by
different people at different times: a policy can arrive from a hub, be
restored from a backup, or be created by a mistyped command, while group
membership is a deliberate act by whoever administers the machine.

This exists because it was needed. A test on a development machine created an
enforcing policy for the logged-in developer, and twelve stray daemons then
locked their screen repeatedly. Nobody was harmed — the password still worked —
but a policy that reaches an adult who was never meant to be managed should not
be able to lock them out of their own computer, and now it cannot.

A failed group lookup counts as *not managed*. When the question cannot be
answered, the answer that does not strand a person wins.

## Fail-safe direction

Every ambiguous case resolves towards the safer outcome for the household, with
one hard exception: **nobody may be locked out by a malfunction.**

- The PAM module returns `PAM_IGNORE` for `root` and for `parents` before it
  contacts the daemon at all.
- It only applies to users a policy explicitly names.
- `on_daemon_unavailable` defaults to `deny` for `kids` and `allow` for everyone
  else.
- An emergency override file readable only by root disables enforcement without
  needing the daemon to run.

## Transparency towards the child

Recording what a person does is only acceptable if that person can see it. The
plasmoid shows the child their own recorded usage, the active policy, and what
is being stored. Window titles are **not** recorded unless a parent switches
`record_window_titles` on.
