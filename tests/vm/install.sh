#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Time Bandits contributors
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Builds and installs Time Bandits from this checkout into a throwaway VM,
# then leaves it in the state a fresh package install would: daemon running,
# PAM untouched, nobody managed, nothing enforced.
#
#     sudo tests/vm/install.sh
#
# Wiring up PAM is deliberately a separate step — see verify.sh, and read
# docs/pam-setup.md before you run it.

cd "$(dirname "$0")/../.." || exit 1
. tests/vm/lib.sh

need_root
need_throwaway_machine

bold "Prerequisites"
missing=""
for tool in cargo make cmake msgfmt; do
    command -v "$tool" >/dev/null || missing="$missing $tool"
done
if [ -n "$missing" ]; then
    die "missing:$missing

  On Fedora KDE:
      dnf install cargo rust make gcc cmake gettext \
                  pam-devel sqlite-devel qt6-qtbase-devel qt6-qtdeclarative-devel
  On Arch:
      pacman -S rust make gcc cmake gettext pam sqlite qt6-base qt6-declarative"
fi
ok "build tools present"

bold "Building"
# As the invoking user, so the build does not leave root-owned files in a
# checkout you will keep editing afterwards.
if [ -n "${SUDO_USER:-}" ]; then
    # A login shell, so a cargo installed through rustup in that user's home is
    # actually on PATH.
    su - "$SUDO_USER" -c "cd '$PWD' && make build" || die "build failed"
else
    make build || die "build failed"
fi
ok "binaries in target/release"

bold "Installing"
make install PREFIX=/usr >/dev/null || die "install failed"
ok "installed under /usr"

# Distributions run these from the package scriptlets; from a `make install`
# they have to be run by hand.
systemd-sysusers >/dev/null 2>&1 || true
systemctl daemon-reload

bold "Groups"
for g in "$TB_TEST_GROUP" parents; do
    if getent group "$g" >/dev/null; then
        ok "group $g exists"
    else
        groupadd "$g" && ok "created group $g"
    fi
done

bold "Daemon"
systemctl enable --now timebanditsd >/dev/null 2>&1 || die "timebanditsd would not start"
sleep 2
systemctl is-active --quiet timebanditsd || {
    journalctl -u timebanditsd -n 30 --no-pager
    die "timebanditsd is not running"
}
ok "timebanditsd running"
[ -S /run/timebandits/pam.sock ] && ok "PAM socket present" || warn "no PAM socket yet"

bold "State"
tbctl doctor || true
cat <<'NEXT'

Installed. Nothing is enforced yet, and PAM has not been touched.

Next:  sudo tests/vm/verify.sh
Undo:  sudo tests/vm/rescue.sh
NEXT
