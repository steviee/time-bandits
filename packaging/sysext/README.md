<!--
SPDX-FileCopyrightText: 2026 Time Bandits contributors
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Time Bandits as a systemd system extension

For image-based systems — Bazzite, Fedora Kinoite and Silverblue, bootc — where
`/usr` is read-only and `rpm-ostree install` rebuilds the deployment on every
update. A system extension is overlaid on `/usr` at boot instead, costs nothing
at update time, and can be switched off again by deleting one file.

```sh
sudo tbctl sysext install    # place the image and merge it
sudo tbctl sysext status     # is it merged, and does it match this OS?
sudo tbctl sysext remove
```

## What is in the image, and what cannot be

A system extension overlays **`/usr` and `/opt` only**. `/etc` is explicitly not
merged, which matters here because three of the things this product needs live
outside `/usr`:

| | where it goes | how it gets there |
|---|---|---|
| binaries, PAM module, units, widget | `/usr` | the image |
| `/etc/pam.d/*` rules | `/etc` | `tbctl pam enable` — `/etc` is writable on ostree |
| `kids` / `parents` groups | `/etc/group` | `systemd-sysusers` |
| `/etc/timebandits/` | `/etc` | written at setup |

So the image is half of an installation, and `tbctl sysext install` does both
halves.

## Three ways this fails quietly, and what catches them

An extension is not a package: it can be absent at the next boot without anybody
doing anything wrong. Each of these looks like "everything is fine" from the
outside, so `tbctl doctor` checks all three.

**The extension does not merge.** Then `pam_timebandits.so` is gone while the
rules naming it are still in `/etc/pam.d`. With a plain `required` control word
that locks *everybody* out of the machine — measured, not assumed. The rules
`tbctl pam enable` writes use `module_unknown=ignore` precisely so this stays
survivable, but the result is still that nothing is enforced, silently.

**A rebase to a new OS version.** An extension whose `extension-release` does
not match the host is refused. We ship `ID=_any`, so a rebase does not silently
unload it; the binaries depend only on glibc, libpam and libsqlite3, and glibc
runs older binaries than itself.

**SELinux, twice over.** The image carries the labels, applied with `setfiles` against the
host policy before the image is built. A mislabelled `pam_timebandits.so`
cannot be loaded by sddm, and — because the module is fail-safe — logins keep
working while enforcement is quietly absent.

## Building one

```sh
make sysext            # needs mksquashfs; SELinux labelling needs root
```

The image is reproducible from a checkout and contains nothing that was not
built from it.


## SELinux: the login services need to be let through

The PAM module does not run in a process of ours. It runs inside whatever is
authenticating — the display manager, the lock screen helper, `login`, `sshd` —
and asks the daemon over a socket in `/run`. Those domains are confined, and
none of them may write to a socket carrying the generic `var_run_t` label.

The failure is quiet in the worst way: the *connect* succeeds and only the
*write* is denied, so the module falls back to its failsafe and refuses a
managed child while everything reports healthy. On Bazzite 44 that meant a child
with no limits set could not log in at all, and the message said the screen-time
service was unavailable.

`packaging/selinux/` carries a policy module that gives `/run/timebandits` a
type of its own and opens only that to the four login domains — rather than
letting them write any socket in `/run`.

```sh
packaging/selinux/build.sh          # needs checkpolicy; build it in a container
sudo semodule -i packaging/selinux/timebandits.pp
sudo systemctl restart timebanditsd # so the socket is created with the new label
```

Two details that cost a debugging round each. The type needs `file_type` and
`non_security_file_type`, or systemd fails to set up `RuntimeDirectory=` with
status 233 and never logs a denial. And `semodule_package` needs `-f` for the
file contexts, or the directory keeps the label the rules do not cover.

`tbctl pam probe` asks the daemon exactly what the module asks, and `tbctl
doctor` now does the same rather than only opening a connection — a check that
connects and stops cannot see this failure at all, which is why it reported a
healthy daemon while nobody could log in.
