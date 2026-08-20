# shellcheck shell=bash
# SPDX-FileCopyrightText: 2026 Time Bandits contributors
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Shared guards and output for the VM walkthrough.

set -euo pipefail

TB_TEST_USER="${TB_TEST_USER:-tbtest}"
TB_TEST_GROUP="${TB_TEST_GROUP:-kids}"

bold()  { printf '\033[1m%s\033[0m\n' "$*"; }
info()  { printf '  %s\n' "$*"; }
ok()    { printf '  \033[32m✓\033[0m %s\n' "$*"; }
warn()  { printf '  \033[33m!\033[0m %s\n' "$*"; }
die()   { printf '\033[31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

need_root() {
    [ "$(id -u)" -eq 0 ] || die "run this as root"
}

# The guard that was missing when a test locked a development machine.
#
# Nothing here may run on a computer somebody depends on. A throwaway VM is
# cheap; an afternoon spent locked out of your own session is not. The override
# exists because testing on spare bare metal is legitimate, but it has to be
# typed out on purpose.
need_throwaway_machine() {
    if [ "${TB_YES_THIS_MACHINE_IS_DISPOSABLE:-}" = "yes" ]; then
        warn "running with the disposable-machine override set"
        return
    fi
    command -v systemd-detect-virt >/dev/null \
        || die "no systemd-detect-virt, so I cannot tell whether this machine is disposable.
  Set TB_YES_THIS_MACHINE_IS_DISPOSABLE=yes if you are certain it is."

    # --quiet, and the exit status, not the output. systemd-detect-virt prints
    # "none" *and* exits 1 on bare metal, so `$(... || echo none)` yields
    # "none\nnone", which compares equal to nothing and waves the guard
    # through. That is how this check failed the first time it was tried.
    local virt
    virt="$(systemd-detect-virt 2>/dev/null || true)"
    if ! systemd-detect-virt --quiet 2>/dev/null; then
        die "this does not look like a VM (systemd-detect-virt says '${virt:-none}').

  These scripts set an enforcing policy and lock a session on purpose. Run them
  in a throwaway VM. If this really is a disposable machine, re-run with:

      TB_YES_THIS_MACHINE_IS_DISPOSABLE=yes $0 $*"
    fi
    ok "throwaway machine ($virt)"
}

# The test user is never you. A policy applied to the account you are sitting
# in is how the incident that motivated these guards happened.
need_separate_test_user() {
    local caller="${SUDO_USER:-${USER:-root}}"
    [ "$TB_TEST_USER" != "$caller" ] || die \
        "TB_TEST_USER is '$TB_TEST_USER', which is the account you are logged in as.
  Pick a different one — these steps deliberately lock it out."
    [ "$TB_TEST_USER" != "root" ] || die "the test user cannot be root"
}

pause() {
    printf '\n  \033[1m%s\033[0m\n' "${1:-Press Enter when done.}"
    read -r _ </dev/tty
}

# Ask the person watching the screen, because some of this cannot be observed
# from a shell.
confirm() {
    local answer
    printf '\n  \033[1m%s\033[0m [y/N] ' "$1"
    read -r answer </dev/tty
    case "$answer" in
        [yY]*) ok "confirmed"; return 0 ;;
        *) die "step failed: $1" ;;
    esac
}
