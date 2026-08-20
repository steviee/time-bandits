#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Time Bandits contributors
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The whole chain, on a real machine, with a person watching the screen.
#
#     sudo tests/vm/verify.sh        # every step, in order
#     sudo tests/vm/verify.sh 5      # one step, to repeat it
#
# Steps 3 and 5 to 8 need a graphical login as the test user, because what is
# being proven is what kcheckpass and SDDM do with our answer — and that cannot
# be observed from a shell. Everything else checks itself.
#
# If anything goes wrong at any point: sudo tests/vm/rescue.sh

cd "$(dirname "$0")/../.." || exit 1
. tests/vm/lib.sh

need_root
need_throwaway_machine
need_separate_test_user

# --- steps -------------------------------------------------------------------

step_1_preconditions() {
    bold "1. Preconditions"
    command -v tbctl >/dev/null || die "tbctl not installed — run tests/vm/install.sh first"
    systemctl is-active --quiet timebanditsd || die "timebanditsd is not running"
    ok "daemon running"
    [ -S /run/timebandits/pam.sock ] || die "no PAM socket at /run/timebandits/pam.sock"
    ok "PAM socket present"
    [ -e /etc/timebandits/disable ] && die "the emergency brake is set — rm /etc/timebandits/disable"
    ok "emergency brake not set"
    # A loop, not `ls pattern1 pattern2`: with two globs, ls fails as soon as
    # *one* of them matches nothing, even when the other found the file.
    # Distributions disagree about the directory, so several patterns is the
    # normal case and the check was failing on a machine that was fine.
    found=""
    for dir in /usr/lib64/security /usr/lib/security /usr/lib/*/security; do
        [ -f "$dir/pam_timebandits.so" ] && { found="$dir/pam_timebandits.so"; break; }
    done
    [ -n "$found" ] || die "PAM module not found in any security directory"
    ok "PAM module installed ($found)"
}

step_2_test_user() {
    bold "2. Test user"
    if id "$TB_TEST_USER" >/dev/null 2>&1; then
        ok "$TB_TEST_USER exists"
    else
        useradd -m -s /bin/bash "$TB_TEST_USER" || die "useradd failed"
        ok "created $TB_TEST_USER"
        info "set a password you can type at a lock screen:"
        passwd "$TB_TEST_USER" </dev/tty
    fi
    usermod -aG "$TB_TEST_GROUP" "$TB_TEST_USER"
    ok "$TB_TEST_USER is in $TB_TEST_GROUP"
    id -nG "$TB_TEST_USER" | tr ' ' '\n' | grep -qx "$TB_TEST_GROUP" || die "group did not take"
}

step_3_observation() {
    bold "3. Measuring, with nothing enforced"
    tbctl policy set "$TB_TEST_USER" --daily 8h --enforcement false >/dev/null
    ok "policy set to observe only"
    info "rules: $(tbctl policy path "$TB_TEST_USER")"

    cat <<EOF

  Now, at the machine:
    - log in graphically as $TB_TEST_USER
    - open two applications, e.g. Konsole and a browser
    - switch between them for two or three minutes, using both
    - leave the session logged in and come back here
EOF
    pause "Press Enter once you have done that."

    echo
    tbctl usage "$TB_TEST_USER"
    echo
    local recorded
    recorded=$(_used_seconds)
    [ "${recorded:-0}" -gt 60 ] || die \
        "only ${recorded:-0}s recorded — nothing is being measured. Check:
    systemctl --user -M ${TB_TEST_USER}@ status timebandits-agent
    journalctl -u timebanditsd -n 50"
    ok "${recorded}s recorded"
    confirm "Does the table above list both applications, with plausible times?"

    info "and the widget:"
    confirm "Does the panel widget show a ring with time remaining?"
}

step_4_pam() {
    bold "4. Wiring up PAM"
    warn "read docs/pam-setup.md before this if you have not"
    tbctl pam enable --dry-run || true
    pause "Review the diff above. Press Enter to apply, or Ctrl+C to stop."
    tbctl pam enable || die "tbctl pam enable failed"
    tbctl pam status
    ok "module installed in the login stacks"

    # Proving the login path still works before anything is enforced, while
    # backing out is still cheap.
    info "checking that an ordinary login still works"
    if command -v pamtester >/dev/null; then
        pamtester login root acct_mgmt >/dev/null 2>&1 \
            && ok "root passes the account stack" \
            || die "root no longer passes — run tests/vm/rescue.sh now"
    else
        warn "pamtester not installed; skipping the automatic check"
    fi
    confirm "Open a second TTY (Ctrl+Alt+F3) and log in as root. Did it work?"
}

step_5_running_out() {
    bold "5. Running out of time"
    info "used so far: $(_used_seconds)s"

    # Two minutes past whatever has already been spent, so the warnings and the
    # lock arrive while somebody is watching rather than immediately.
    tbctl policy set "$TB_TEST_USER" \
        --enforcement true --daily "$(_used_plus_minutes 2)" >/dev/null
    ok "quota set to two minutes from now, enforcement on"
    tbctl status "$TB_TEST_USER"

    cat <<EOF

  Switch to the $TB_TEST_USER session and watch. Within two minutes you should see:
    - a desktop notification warning that time is running out
    - an offer to ask for more time
    - the session locking on its own
EOF
    pause "Press Enter when the session has locked."
    loginctl list-sessions --no-legend | awk -v u="$TB_TEST_USER" '$3==u {print $1}' | while read -r s; do
        info "session $s LockedHint=$(loginctl show-session "$s" -p LockedHint --value)"
    done
    confirm "Did a warning appear, and did the session lock by itself?"
}

step_6_cannot_unlock() {
    bold "6. The lock screen must refuse the correct password"
    cat <<EOF

  At the lock screen, type ${TB_TEST_USER}'s correct password and press Enter.
  This is the step that separates us from a screensaver: the password is right
  and it must still be refused, with a sentence explaining why and when.
EOF
    pause "Press Enter when you have tried it."
    confirm "Was the correct password refused, with a readable explanation?"
    confirm "Did the message say when time is available again?"
}

step_7_cannot_log_in_again() {
    bold "7. Logging in again must fail too"
    info "ending the session, as a determined child would"
    loginctl list-sessions --no-legend | awk -v u="$TB_TEST_USER" '$3==u {print $1}' \
        | xargs -r -n1 loginctl terminate-session
    sleep 2
    ok "session ended; SDDM should be showing a login screen"

    if command -v pamtester >/dev/null; then
        pamtester sddm "$TB_TEST_USER" acct_mgmt >/dev/null 2>&1 \
            && die "the account stack still permits $TB_TEST_USER — enforcement is not working" \
            || ok "the account stack refuses $TB_TEST_USER"
        pamtester sddm root acct_mgmt >/dev/null 2>&1 \
            && ok "root is unaffected" \
            || die "root is being refused — run tests/vm/rescue.sh now"
    fi

    pause "At the login screen, log in as $TB_TEST_USER with the correct password."
    confirm "Was the login refused, with the same explanation?"
}

step_8_bonus() {
    bold "8. More time, immediately"
    tbctl grant-bonus "$TB_TEST_USER" 15m
    ok "granted fifteen minutes"
    sleep 6
    tbctl status "$TB_TEST_USER"
    [ "$(_status_field allowed)" = "true" ] \
        || die "the daemon still says no after a bonus — enforcement is stuck"
    ok "the daemon permits $TB_TEST_USER again"
    if command -v pamtester >/dev/null; then
        pamtester sddm "$TB_TEST_USER" acct_mgmt >/dev/null 2>&1 \
            && ok "the account stack permits $TB_TEST_USER again" \
            || die "still refused after a bonus — enforcement is stuck"
    fi
    pause "Log in as $TB_TEST_USER again."
    confirm "Did the login work, without anything being restarted?"
}

step_9_tamper() {
    bold "9. Switching off the reporting does not stop the clock"
    local before after
    before=$(_used_seconds)
    info "used: ${before}s"

    pkill -x timebandits-agent 2>/dev/null && ok "killed the agent" || warn "no agent running"
    # -x again: a -f pattern here would match this script.
    su "$TB_TEST_USER" -c 'kwriteconfig6 --file kwinrc --group Plugins \
        --key org.timebandits.focusEnabled false' 2>/dev/null \
        && ok "disabled the KWin script" || warn "could not reach kwinrc"

    info "waiting a minute to see whether time still counts"
    sleep 60
    after=$(_used_seconds)
    info "used: ${after}s"
    [ "$after" -gt "$before" ] || die "time stopped counting — this is fail-open, and wrong"
    ok "time kept counting with the reporters gone"

    journalctl -u timebanditsd --since '-2min' --no-pager | grep -i tamper \
        && ok "a tamper event was logged" || warn "no tamper line in the journal"
}

step_10_emergency_brake() {
    bold "10. The emergency brake"
    touch /etc/timebandits/disable
    ok "set /etc/timebandits/disable"
    sleep 6
    tbctl doctor | grep -i 'emergency\|override' || true
    if command -v pamtester >/dev/null; then
        pamtester sddm "$TB_TEST_USER" acct_mgmt >/dev/null 2>&1 \
            && ok "everyone is permitted again, without touching PAM" \
            || die "the brake did not release enforcement"
    fi
    confirm "Can $TB_TEST_USER log in now, with an exhausted quota?"
    rm -f /etc/timebandits/disable
    ok "brake released again"
}

step_11_done() {
    bold "11. Done"
    tbctl doctor || true
    cat <<EOF

  Everything above that passed, held on a real system rather than in a test.

  Leave the machine as it is if you want to keep poking at it. To put it back:
      sudo tests/vm/rescue.sh
EOF
}

# --- helpers -----------------------------------------------------------------

# Read one field out of `tbctl status --json`. Scraping the human report would
# be the kind of check that silently succeeds against the wrong number.
_status_field() {
    tbctl status "$TB_TEST_USER" --json \
        | python3 -c "
import json, sys
v = json.load(sys.stdin)['$1']
# json.dumps so a boolean comes out as true/false rather than Python's True.
print('' if v is None else json.dumps(v).strip('\"'))"
}

_used_seconds() { _status_field used_today_secs; }

_used_plus_minutes() {
    echo "$(( $(_used_seconds) / 60 + $1 ))m"
}

# --- runner ------------------------------------------------------------------

STEPS=(
    step_1_preconditions
    step_2_test_user
    step_3_observation
    step_4_pam
    step_5_running_out
    step_6_cannot_unlock
    step_7_cannot_log_in_again
    step_8_bonus
    step_9_tamper
    step_10_emergency_brake
    step_11_done
)

if [ $# -gt 0 ]; then
    for n in "$@"; do
        [ "$n" -ge 1 ] && [ "$n" -le "${#STEPS[@]}" ] || die "no step $n"
        "${STEPS[$((n - 1))]}"
    done
else
    for s in "${STEPS[@]}"; do
        "$s"
        echo
    done
fi
