#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Time Bandits contributors
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Removes the system extension. Takes the PAM rules out first, because a rule
# naming a module that is about to disappear is the one order that matters.

set -euo pipefail
[ "$(id -u)" -eq 0 ] || { echo "run as root" >&2; exit 1; }

if command -v tbctl >/dev/null; then
    tbctl pam disable || true
fi
systemctl disable --now timebanditsd 2>/dev/null || true
systemd-sysext unmerge || true
rm -f /var/lib/extensions/timebandits.raw
systemctl daemon-reload
echo "removed; /etc/timebandits and the recorded usage are kept"
