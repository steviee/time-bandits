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

**SELinux.** The image carries the labels, applied with `setfiles` against the
host policy before the image is built. A mislabelled `pam_timebandits.so`
cannot be loaded by sddm, and — because the module is fail-safe — logins keep
working while enforcement is quietly absent.

## Building one

```sh
make sysext            # needs mksquashfs; SELinux labelling needs root
```

The image is reproducible from a checkout and contains nothing that was not
built from it.
