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
