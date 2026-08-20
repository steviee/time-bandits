#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Time Bandits contributors
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Tests for the guards in lib.sh, because they are the most safety-critical
# shell in the repository and the VM check was wrong the first time it ran:
# systemd-detect-virt prints "none" *and* exits 1, so capturing its output with
# a `|| echo none` fallback produced "none\nnone", which matched nothing and
# waved a bare-metal machine straight through.
#
# Runs anywhere, changes nothing, needs no root.

cd "$(dirname "$0")" || exit 1
set -uo pipefail

pass=0
fail=0

check() {
    local name="$1"; shift
    if "$@" >/dev/null 2>&1; then
        printf '  \033[32m✓\033[0m %s\n' "$name"; pass=$((pass + 1))
    else
        printf '  \033[31m✗\033[0m %s\n' "$name"; fail=$((fail + 1))
    fi
}

refuses() {
    local name="$1"; shift
    if "$@" >/dev/null 2>&1; then
        printf '  \033[31m✗\033[0m %s (it allowed this)\n' "$name"; fail=$((fail + 1))
    else
        printf '  \033[32m✓\033[0m %s\n' "$name"; pass=$((pass + 1))
    fi
}

# A stand-in for systemd-detect-virt that answers however we like.
with_virt() {
    local answer="$1"; shift
    local dir; dir="$(mktemp -d)"
    if [ "$answer" = "absent" ]; then
        # An empty PATH entry plus a PATH that cannot find the real one.
        PATH="$dir" "$@"
    else
        cat >"$dir/systemd-detect-virt" <<EOF
#!/bin/sh
if [ "\$1" = "--quiet" ]; then
    [ "$answer" != "none" ] && exit 0 || exit 1
fi
echo "$answer"
[ "$answer" != "none" ] && exit 0 || exit 1
EOF
        chmod +x "$dir/systemd-detect-virt"
        PATH="$dir:$PATH" "$@"
    fi
    local status=$?
    rm -rf "$dir"
    return $status
}

guard() {
    env -u TB_YES_THIS_MACHINE_IS_DISPOSABLE \
        bash -c '. "'"$PWD"'/lib.sh"; need_throwaway_machine'
}

guard_with_override() {
    TB_YES_THIS_MACHINE_IS_DISPOSABLE=yes \
        bash -c '. "'"$PWD"'/lib.sh"; need_throwaway_machine'
}

test_user_guard() {
    TB_TEST_USER="$1" SUDO_USER="$2" \
        bash -c '. "'"$PWD"'/lib.sh"; need_separate_test_user'
}

echo "guards:"

# The one that failed. Bare metal must be refused, and the message must not be
# the only thing standing between a test and somebody's working machine.
refuses "bare metal is refused"                 with_virt none    guard
refuses "an unknown answer is refused"          with_virt absent  guard
check   "a KVM guest is allowed"                with_virt kvm     guard
check   "a container is allowed"                with_virt podman  guard
check   "the override works on bare metal"      with_virt none    guard_with_override

# The other half of the incident: a policy applied to the account you are
# sitting in.
refuses "the caller cannot be the test user"    test_user_guard alice alice
refuses "root cannot be the test user"          test_user_guard root  alice
check   "a separate account is fine"            test_user_guard tbtest alice

echo
if [ "$fail" -gt 0 ]; then
    printf '\033[31m%d of %d failed\033[0m\n' "$fail" "$((pass + fail))"
    exit 1
fi
printf '\033[32mall %d passed\033[0m\n' "$pass"
