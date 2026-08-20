<!--
SPDX-FileCopyrightText: 2026 Time Bandits contributors
SPDX-License-Identifier: GPL-3.0-or-later
-->

# PAM integration

This is the part of Time Bandits that can lock people out of their own machine
if it is done wrong. Read this page before touching `/etc/pam.d` by hand; in
normal use `tbctl pam enable` does all of it, with a backup and a dry run.

## Where the module goes, and why

| File | Stack | Line |
|---|---|---|
| `/etc/pam.d/kde` | `auth` | `auth requisite pam_timebandits.so` |
| `/etc/pam.d/sddm` | `account` | `account required pam_timebandits.so` |
| `/etc/pam.d/login` | `account` | `account required pam_timebandits.so` |

KScreenLocker evaluates **only the `auth` stack** of the `kde` service. That is
the whole reason an expired quota can prevent a child from simply unlocking with
their own password — an approach based on logind alone cannot do this, which is
the known gap in timekpr-nExT.

The module must be listed **before** the module that checks the password, so the
refusal appears without a pointless password prompt. `requisite` makes the stack
stop right there.

## Marker format

Lines added to `/etc/pam.d` are wrapped in markers:

```
# >>> time-bandits >>>
auth     requisite   pam_timebandits.so
# <<< time-bandits <<<
```

`tbctl pam enable` writes this block, `tbctl pam disable` removes it, and the
package uninstall scriptlets remove it too. `tbctl pam enable --dry-run` shows
the changes without making them, and every file is backed up alongside itself
with a `.timebandits-backup` suffix before its first edit — the backup is taken
once, so it always shows the system as it was before this tool ever touched it. That last one is not a nicety: a
`required` line pointing at a module that no longer exists makes PAM fail, and
nobody can log in. Anything that edits these files must use the same markers.

## Options

```
pam_timebandits.so [socket=PATH] [timeout_ms=N] [fallback=deny|allow]
                   [managed_group=NAME] [exempt_group=NAME] [debug]
```

| Option | Default | Meaning |
|---|---|---|
| `socket` | `/run/timebandits/pam.sock` | Where the daemon listens |
| `timeout_ms` | `300` | Clamped to 50–5000 ms |
| `fallback` | `deny` | What happens for managed users when the daemon is unreachable |
| `managed_group` | `kids` | Only these users are subject to the fallback |
| `exempt_group` | `parents` | Never touched, whatever the daemon says |
| `debug` | off | Log decisions to `authpriv` |

A misspelled option is ignored rather than fatal.

## Safety properties

1. **The module can only refuse.** Its result type has three variants —
   `Ignore`, `AuthError`, `PermissionDenied` — and no way to express
   `PAM_SUCCESS`. Even if someone writes `sufficient` instead of `requisite`,
   the module cannot become a bypass for the password check.
2. **`root` and `parents` are checked first**, before the daemon is contacted at
   all. A failed group lookup counts as exempt: locking a parent out because NSS
   hiccuped is the worse failure.
3. **Panics fail open.** Every entry point catches unwinds and returns
   `PAM_IGNORE`. An unreachable daemon is an adversarial condition and fails
   closed for managed users; a bug in our own code is our fault and must not
   cost the household their machines.
4. **Bounded time.** The exchange runs on a separate thread with a deadline, so
   a wedged daemon cannot freeze the greeter — not even through a blocking
   `connect` on a full accept backlog.

## Getting back in

If the module ever refuses when it should not:

1. Switch to a text console with `Ctrl+Alt+F3` and log in as a parent or root —
   they are exempt before any of this logic runs.
2. `tbctl pam disable` restores the backed-up files.
3. If the daemon itself is the problem: `systemctl stop timebanditsd` and create
   `/etc/timebandits/disable`, which suppresses enforcement even on restart.
4. As a last resort, boot with `systemd.unit=rescue.target` and remove the
   `pam_timebandits.so` lines.

## After a distribution update

`sddm` and `kscreenlocker` ship their PAM files as `%config(noreplace)`, so
edits survive an update but a `.rpmnew` may appear. `timebanditsd` verifies the
markers at every start and warns; `tbctl doctor` reports and repairs.

## Testing changes

Never test on a machine you are currently logged into as the only user.

```sh
podman run --rm -it --systemd=always fedora:44 /sbin/init   # throwaway target
pamtester tb-test testkid acct_mgmt
```

The module's own test suite covers the decision table — root, parents,
unmanaged users, denial, unreachable daemon, garbled answer, and both fallback
settings — without needing a PAM stack at all.


## Which services get a rule, and why the list is longer than it looks

`tbctl pam enable` configures every service below that exists on the machine,
and skips the rest.

| Service | Stack | What it guards |
|---|---|---|
| `kde` | auth | the lock screen; KScreenLocker evaluates only the auth stack |
| `kcheckpass` | auth | the lock screen's password helper on some builds |
| `sddm` | account | the display manager, before Plasma 6.5 |
| `plasmalogin` | account | the display manager, Plasma 6.5 and later |
| `login` | account | text console login |

Two display managers and two lock-screen services, because which one is live
has moved between releases. Covering a service nothing uses costs nothing;
missing the one in use costs everything — on Bazzite 44 there is no `sddm` at
all, and a tool that only knew about that name configured no login screen
while reporting success.

### The vendor directory

Fedora keeps some service files in `/usr/lib/pam.d` and not in `/etc/pam.d` —
`plasmalogin` is one. Files in `/etc/pam.d` override same-named ones there,
which is also the documented way to customise them, so `tbctl pam enable`
copies such a file into `/etc/pam.d` before adding its rule. Editing the vendor
file in place would work until the next package update silently replaced it,
taking the rule with it.

### If the module ever goes missing

The rules use PAM's bracket form rather than `required` or `requisite`:

```
auth [success=ok new_authtok_reqd=ok ignore=ignore module_unknown=ignore default=die] pam_timebandits.so
```

`module_unknown=ignore` steps over the module when it is not installed. With a
plain control word, deleting `pam_timebandits.so` while its rules are still in
place locks **every** account out of the machine — measured on Fedora 44, and
the leading-dash form does not help, it only suppresses the log entry.

That matters after an uninstall that skipped `tbctl pam disable`, and it
matters routinely on image-based systems, where a system extension can fail to
merge on any boot.
