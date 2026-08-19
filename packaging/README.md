<!--
SPDX-FileCopyrightText: 2026 Time Bandits contributors
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Packaging

## Two audiences, one file layout

There are two different things called "packaging" here, and conflating them
produces recipes that satisfy neither:

**Upstream convenience packages** — what lives in this directory. They let
people install Time Bandits today, from COPR, the AUR, or a downloaded `.deb`.
They vendor their Rust dependencies because that is the only way to build
offline without every crate already being packaged by the distribution.

**Distribution-native packages** — what a Fedora or Debian maintainer will
write when the project is submitted for inclusion. Those follow that
distribution's own rules: Fedora's `%cargo_*` macros, Debian's `dh-cargo` and
separately packaged crates. That work belongs to the distribution, not here,
and trying to pre-empt it produces a spec that is wrong for both purposes.

What the two share is the **file layout**, which lives in the top-level
`Makefile`. Every recipe calls `make install` rather than listing files itself.
Three hand-maintained file lists is exactly how a new file lands in the Arch
package and goes missing from the Debian one.

```
/usr/bin/timebanditsd
$PAMDIR/pam_timebandits.so          # differs per distribution, see below
/usr/lib/systemd/system/timebanditsd.service
/etc/timebandits/daemon.toml        # config, never replaced on upgrade
/var/lib/timebandits/               # created by systemd's StateDirectory=
/run/timebandits/                   # created by systemd's RuntimeDirectory=
```

## The PAM module path differs everywhere

| Distribution | Path |
|---|---|
| Arch | `/usr/lib/security` |
| Fedora, RHEL, openSUSE | `/usr/lib64/security` |
| Debian, Ubuntu | `/usr/lib/<triplet>/security` |

Each recipe passes `PAMDIR=` explicitly. Getting this wrong produces a package
that installs cleanly and then does nothing, because PAM simply never finds the
module.

## Uninstalling must remove the PAM lines

Every recipe removes its own `/etc/pam.d` lines on uninstall, keyed on markers:

```
# >>> time-bandits >>>
auth     requisite   pam_timebandits.so
# <<< time-bandits <<<
```

This is not tidiness. A `required` line pointing at a module that no longer
exists makes PAM fail, and then **nobody can log in**. Removing the lines while
the module still exists is the only safe order, which is why it happens in
`pre_remove` / `%preun` / `prerm` rather than after.

## Declared dependencies match reality

Checked with `ldd` against the built artifacts:

```
timebanditsd          libsqlite3, libgcc_s, libm, libc
pam_timebandits.so    libpam, libgcc_s, libc  (+ libaudit, libcap-ng via libpam)
```

Notably **no libsystemd**: D-Bus goes through `zbus`, which is pure Rust. An
early draft of the PKGBUILD declared `systemd-libs` out of habit; the `ldd`
check is what caught it.

`rusqlite` is built without its `bundled` feature on purpose. Linking the
system SQLite is what keeps the package acceptable to distributions, which do
not permit bundled copies of libraries.

## Minimum Rust version

**1.90**, verified rather than assumed: the workspace builds and all tests pass
on it, and CI runs a job that would fail if that stopped being true.

This matters more than it looks. The code needs edition 2024 and let-chains,
which rules out anything before 1.88 — and Debian stable lags far enough behind
that the upstream `.deb` targets Debian unstable and current Ubuntu rather than
the current stable release.

## Debug information

None of these packages ship usable debug symbols, and the recipes disable the
distributions' debug packages (`options=('!debug')`, `%global debug_package
%nil`) rather than shipping empty ones.

The reason is Cargo, not the packaging: the release profile builds without
debug information at all, so there is nothing for `rpmbuild` or `makepkg` to
split out. A distribution that wants real `-debuginfo` / `-dbgsym` packages
should build with

```sh
RUSTFLAGS="-Cdebuginfo=2" make build
```

and drop the corresponding opt-out. That is worth doing for a daemon that
enforces things — a useful backtrace from a parent's bug report is worth the
build time — and it is on the list once the packages leave early development.

## Building the packages

```sh
make dist            # produces time-bandits-<version>.tar.gz and vendor.tar.zst

# Arch
cd packaging/arch && makepkg -si

# Fedora / RHEL / openSUSE
rpmbuild -ba packaging/rpm/time-bandits.spec

# Debian / Ubuntu
cp -r packaging/debian debian && dpkg-buildpackage -us -uc -b
```

CI builds all three on every push, because a packaging recipe that is not built
is a packaging recipe that is broken.

## Not yet packaged

The session agent, the KWin script, the plasmoid and the household server are
still being built. They will become separate binary packages
(`time-bandits-plasma`, `time-bandits-hub`).

The split is not about saving disk space. **The enforcing half is not
KDE-specific at all**: it is built on logind and PAM, which every systemd
desktop has. The daemon measures session time and locks sessions on GNOME,
Sway or XFCE exactly as it does on Plasma. Only three pieces are tied to KDE —
the KWin script that reports which window has focus, the plasmoid, and the
notification path in the agent.

Keeping those in `time-bandits-plasma` means a GNOME household can install the
base package today and get quotas, time windows and lockout, losing only the
per-application breakdown. It also means the equivalent front end for another
desktop is an added package rather than a fork.

`time-bandits-hub` is the genuinely headless one: it belongs on a Raspberry Pi
or a NAS and shares nothing with the client beyond the wire protocol.

`tbctl` is not shipped yet either, which is why PAM setup is currently a
documented manual step rather than one command.
