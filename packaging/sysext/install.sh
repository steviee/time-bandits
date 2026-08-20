#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Time Bandits contributors
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Installs the system extension, and undoes it by itself if anything is wrong.
#
#     sudo packaging/sysext/install.sh timebandits.raw
#
# Merging an extension is not like installing a package. It relabels merged
# directories, it happens again at every boot, and a mistake in it can leave a
# machine with no way to log in — which is not a story, it is what happened
# while this was being written. So the merge here is guarded three ways:
#
#   1. the labels in the image are checked against the host before anything
#      is touched,
#   2. an unmerge is *scheduled before the merge*, and only cancelled once the
#      checks pass — so a machine that stops answering repairs itself,
#   3. authentication is exercised after the merge, because "the files look
#      right" is not the same as "somebody can still log in".

set -euo pipefail
cd "$(dirname "$0")/../.." || exit 1
# shellcheck source=packaging/sysext/labels.sh
. packaging/sysext/labels.sh

IMG="${1:-timebandits.raw}"
NAME=timebandits
DEST=/var/lib/extensions
GRACE="${TB_SYSEXT_GRACE:-120}"

[ "$(id -u)" -eq 0 ] || { echo "run as root" >&2; exit 1; }
[ -f "$IMG" ] || { echo "no image at $IMG" >&2; exit 1; }
command -v systemd-sysext >/dev/null || { echo "no systemd-sysext here" >&2; exit 1; }

# --- 1. before touching anything --------------------------------------------

echo "checking the image against this host"
insp="$(mktemp -d)"
trap 'umount "$insp" 2>/dev/null || true; rmdir "$insp" 2>/dev/null || true' EXIT
mount -o loop,ro "$IMG" "$insp"

if [ -f /etc/selinux/targeted/contexts/files/file_contexts ]; then
    ok=1
    check_labels_against_host "$insp" || ok=0
    [ "$ok" = 1 ] || {
        echo >&2
        echo "This image would relabel paths the whole system depends on." >&2
        echo "Not merging it. Rebuild with packaging/sysext/build.sh." >&2
        exit 1
    }
else
    echo "no SELinux here; skipping the label check"
fi
umount "$insp"

# --- 2. arrange the way out, before creating the problem ---------------------

# Scheduled *first*. If the next steps wedge this machine, it comes back on its
# own instead of needing a console and a kernel command line.
echo "scheduling an automatic undo in ${GRACE}s"
systemd-run --quiet --unit=tb-sysext-rollback --on-active="$GRACE" \
    /bin/sh -c "systemd-sysext unmerge; rm -f $DEST/$NAME.raw" >/dev/null
cancel_rollback() { systemctl stop tb-sysext-rollback.timer 2>/dev/null || true; }

# --- 3. merge ----------------------------------------------------------------

install -Dm0644 "$IMG" "$DEST/$NAME.raw"
systemd-sysext merge
systemd-sysext status

# --- 4. prove the machine still works ----------------------------------------

fail() {
    echo >&2
    echo "!! $1" >&2
    echo "Undoing the merge now." >&2
    systemd-sysext unmerge || true
    rm -f "$DEST/$NAME.raw"
    cancel_rollback
    exit 1
}

echo "checking that authentication still works"
# The failure that made this script necessary: a relabelled
# /usr/lib64/security stopped *every* process from loading *any* PAM module.
# `su` goes through the full PAM stack, so it notices.
su -s /bin/true nobody 2>/dev/null || fail "PAM can no longer authenticate"
echo "  PAM still works"

[ -f /usr/lib64/security/pam_timebandits.so ] || fail "the PAM module did not appear"
echo "  the module is in place"

/usr/bin/timebanditsd --version >/dev/null 2>&1 || fail "the daemon will not run"
echo "  the daemon runs"

cancel_rollback
systemctl daemon-reload

cat <<NEXT

Merged, and the automatic undo has been cancelled.

The image is /usr only. The rest of an installation lives in /etc, which a
system extension never covers:

    sudo systemd-sysusers                 # the kids and parents groups
    sudo systemctl enable --now timebanditsd
    sudo tbctl pam enable                 # read docs/pam-setup.md first
    sudo tbctl doctor

To remove it again:  sudo packaging/sysext/uninstall.sh
NEXT
