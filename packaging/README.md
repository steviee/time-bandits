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

## Package structure

Three packages, and the line between them is drawn by what actually depends on
a desktop:

```
  time-bandits ─────────────────┐        time-bandits-hub
  timebanditsd                  │        timebandits-hub
  pam_timebandits.so            │        (Raspberry Pi, NAS, container)
  timebandits-agent             │
  tbctl                         │        shares only the wire protocol
    │                           │
    │ required by               │
    ▼                           │
  time-bandits-plasma           │        time-bandits-gnome   (later)
  KWin focus script             │        Shell extension
  plasmoid                      │        panel indicator
```

**`time-bandits`** is the whole enforcing system and depends on no desktop at
all. logind and PAM are what it stands on, and every systemd desktop has both.
The session agent lives here too, because most of what it does is portable:
idle detection is `ext-idle-notify-v1`, a Wayland protocol, and notifications
go through `org.freedesktop.Notifications`, a freedesktop specification. Its
dependencies are `pam` and `sqlite` — nothing else.

**`time-bandits-plasma`** is what a KDE household installs, and it pulls the
core in. It contains the two genuinely KDE-specific pieces: the KWin script
that reports which window has focus, and the plasmoid.

That single file — the focus reporter — is the entire desktop-specific surface.
Wayland has no general way for one application to observe another's windows, so
each compositor needs its own answer: a KWin script here, a GNOME Shell
extension later. Everything upstream of it, from the app-identity
normalisation to the reports, is shared.

**`time-bandits-hub`** is the household server. It shares nothing with the
client beyond the versioned wire protocol and can sit on a different
architecture and a different release cycle.

### Why not one package

A single package would make the daemon depend on Plasma, which would be a lie:
the daemon locks GNOME and Sway sessions perfectly well and always could. It
would also make a GNOME front end a fork rather than an added package, which is
the difference between someone contributing one and someone not bothering.

## Not yet packaged

Only `time-bandits` exists today; the daemon, the PAM module and the session
agent are what ship. `time-bandits-plasma` and `time-bandits-hub` are added to
these recipes as the code for them lands, in the same commit — packaging that
trails the code is packaging that is quietly broken.

`tbctl` is not shipped yet either, which is why PAM setup is currently a
documented manual step rather than one command.
