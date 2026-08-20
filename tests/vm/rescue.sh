#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Time Bandits contributors
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Undoes everything the walkthrough did. Written to work from a plain TTY with
# nothing else running: if PAM ever refuses a login you did not expect, this is
# what you reach for.
#
#     Ctrl+Alt+F3, log in as root, then:
#     /path/to/checkout/tests/vm/rescue.sh
#
# The first thing it does is pull the emergency brake, so enforcement stops
# before anything slower is attempted.

cd "$(dirname "$0")/../.." || exit 1
. tests/vm/lib.sh

need_root

bold "1. Emergency brake"
mkdir -p /etc/timebandits
touch /etc/timebandits/disable
ok "/etc/timebandits/disable — enforcement is off as of now"

bold "2. PAM"
if tbctl pam disable 2>/dev/null; then
    ok "module removed from the login stacks"
else
    warn "tbctl could not do it; falling back to grep"
    for f in /etc/pam.d/*; do
        if grep -q pam_timebandits "$f" 2>/dev/null; then
            sed -i '/pam_timebandits/d' "$f"
            ok "cleaned $f by hand"
        fi
    done
fi
grep -rl pam_timebandits /etc/pam.d/ 2>/dev/null && die "still referenced above" || \
    ok "no references left in /etc/pam.d"

bold "3. Daemon"
systemctl disable --now timebanditsd >/dev/null 2>&1 || true
pkill -x timebanditsd 2>/dev/null || true
# -x, matching the process name only. A -f pattern would also match this
# script's own command line, kill the script, and leave the daemons running —
# which is exactly how a development machine once ended up locked.
sleep 1
pgrep -x timebanditsd >/dev/null && warn "a daemon is still running" || ok "no daemon running"

bold "4. Rules"
rm -f /etc/timebandits/policy.d/*.toml 2>/dev/null || true
ok "policies removed (usage database left in place)"

bold "5. Test user"
if id "$TB_TEST_USER" >/dev/null 2>&1; then
    gpasswd -d "$TB_TEST_USER" "$TB_TEST_GROUP" >/dev/null 2>&1 || true
    ok "$TB_TEST_USER removed from $TB_TEST_GROUP"
    info "the account itself is kept; userdel -r $TB_TEST_USER removes it"
fi

cat <<'DONE'

The machine is back to normal. Logins are unaffected by Time Bandits.

To pick the walkthrough back up, delete the brake and start the daemon:
    rm /etc/timebandits/disable && systemctl start timebanditsd
DONE
