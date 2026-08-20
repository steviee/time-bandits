#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Time Bandits contributors
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The PAM module, against a real libpam, in a throwaway container.
#
#     tests/pam/integration.sh            # builds in a Fedora container
#     tests/pam/integration.sh --inside   # what runs inside it
#
# Unit tests cover the module's decisions. They cannot cover what libpam does
# with those decisions — which control word to use, what happens when the
# module file is missing, whether root stays exempt. That needs a real PAM
# stack, and getting it wrong locks people out of their computers.
#
# This found two real faults the first time it ran: a plain `requisite` control
# word locking every account out when the module was absent, and the group
# check being applied by the tick loop but not by the responder.

set -euo pipefail

IMAGE="${TB_TEST_IMAGE:-registry.fedoraproject.org/fedora:44}"
SEC=/usr/lib64/security

# --- the part that runs inside the container ---------------------------------

if [ "${1:-}" = "--inside" ]; then
    fail=0
    # Not swallowed: a failed install here looks exactly like a broken harness,
    # and cost a CI run to work out.
    dnf install -y -q pamtester || { echo "could not install pamtester"; exit 1; }
    command -v pamtester >/dev/null || { echo "pamtester is not on PATH"; exit 1; }

    install -Dm0755 /stage/timebanditsd /usr/bin/timebanditsd
    install -Dm0755 /stage/tbctl        /usr/bin/tbctl
    install -Dm0755 /stage/libpam_timebandits.so "$SEC/pam_timebandits.so"

    groupadd kids
    useradd -m -G kids kid;   echo 'kid:secret'   | chpasswd
    useradd -m grown;         echo 'grown:secret' | chpasswd

    mkdir -p /etc/timebandits/policy.d /var/lib/timebandits /run/timebandits
    cat > /etc/timebandits/daemon.toml <<'CFG'
state_dir = "/var/lib/timebandits"
policy_dir = "/etc/timebandits/policy.d"
managed_group = "kids"
CFG

    # The exact lines tbctl writes, so this tests what is actually deployed
    # rather than a copy that can drift. A bare container has neither service,
    # and tbctl skips what is not installed, so give it something to edit.
    for svc in kde sddm; do
        printf '#%%PAM-1.0\nauth include system-auth\naccount include system-auth\n' \
            > "/etc/pam.d/$svc"
    done
    tbctl pam enable --root /etc/pam.d --config /etc/timebandits/daemon.toml >/dev/null
    AUTH=$(grep -oP '(?<=^auth ).*(?= pam_timebandits\.so)' /etc/pam.d/kde 2>/dev/null || echo '')
    [ -n "$AUTH" ] || { echo "could not read the control word tbctl writes"; exit 1; }
    ACCT=$(grep -oP '(?<=^account ).*(?= pam_timebandits\.so)' /etc/pam.d/sddm 2>/dev/null \
        || echo "$AUTH")
    # pam_permit stands in for "the password checked out". Our module never
    # verifies a password — it decides whether someone may proceed — so putting
    # pam_unix underneath would only make the test depend on the container
    # having a working crypt and shadow file. One CI runner does not, and
    # failed the whole job before any of our code ran.
    cat > /etc/pam.d/tb <<P
auth     $AUTH  pam_timebandits.so
auth     required     pam_permit.so
account  $ACCT  pam_timebandits.so
account  required     pam_permit.so
P
    cat > /etc/pam.d/tb-control <<'P'
auth     required     pam_permit.so
account  required     pam_permit.so
P
    # The same stack over a real pam_unix, run only where the environment can
    # actually check a password. Weaker guarantees, stronger evidence.
    cat > /etc/pam.d/tb-unix <<P
auth     $AUTH  pam_timebandits.so
auth     required     pam_unix.so
account  $ACCT  pam_timebandits.so
account  required     pam_unix.so
P

    # expect <allow|deny> <user> <phase> <description>
    expect() {
        local want="$1" user="$2" phase="$3" desc="$4" got
        if echo secret | pamtester "${SERVICE:-tb}" "$user" "$phase" >/dev/null 2>&1; then
            got=allow
        else
            got=deny
        fi
        if [ "$got" = "$want" ]; then
            printf '  \033[32m✓\033[0m %s\n' "$desc"
        else
            printf '  \033[31m✗\033[0m %s — wanted %s, got %s\n' "$desc" "$want" "$got"
            fail=$((fail + 1))
        fi
    }

    echo "harness:"
    if out=$(echo secret | pamtester tb-control kid authenticate 2>&1); then
        printf '  \033[32m✓\033[0m pamtester runs a stack and reports its result\n'
    else
        echo "  the harness itself is broken, before any of our code is involved:"
        echo "  $out"
        echo "  --- /etc/pam.d/tb-control"; sed 's/^/  /' /etc/pam.d/tb-control
        echo "  --- the user";        id kid
        echo "  --- shadow entry";    getent shadow kid | cut -c1-24
        echo "  --- who am I";        id
        echo "  --- capabilities";    grep CapEff /proc/self/status
        echo "  --- confinement";     cat /proc/self/attr/current 2>/dev/null || echo none
        echo "  --- shadow readable"; head -c1 /etc/shadow >/dev/null 2>&1 && echo yes || echo no
        echo "  --- unix_chkpwd";     ls -l /usr/sbin/unix_chkpwd 2>&1
        exit 1
    fi

    timebanditsd --config /etc/timebandits/daemon.toml >/tmp/daemon.log 2>&1 &
    dpid=$!
    for _ in $(seq 1 40); do [ -S /run/timebandits/pam.sock ] && break; sleep 0.25; done
    [ -S /run/timebandits/pam.sock ] || { echo "daemon did not start"; tail -5 /tmp/daemon.log; exit 1; }

    echo "no policy:"
    expect allow kid   authenticate "an unmanaged child may unlock"
    expect allow grown acct_mgmt    "an adult may log in"

    echo "quota used up:"
    tbctl policy set kid --daily 0 --enforcement true >/dev/null
    expect deny  kid   authenticate "the lock screen refuses the correct password"
    expect deny  kid   acct_mgmt    "a fresh login is refused"
    expect allow grown acct_mgmt    "the adult beside them is unaffected"
    expect allow root  acct_mgmt    "root is never refused"

    echo "same policy, but the user is not in the managed group:"
    gpasswd -d kid kids >/dev/null
    expect allow kid   acct_mgmt    "a policy alone does not authorise refusing anybody"
    gpasswd -a kid kids >/dev/null

    echo "daemon gone:"
    kill "$dpid" 2>/dev/null || true
    sleep 1; rm -f /run/timebandits/pam.sock
    expect deny  kid   acct_mgmt    "a managed child is held (fail-closed)"
    expect allow grown acct_mgmt    "everyone else is let through (fail-open)"

    echo "module gone — package removed, or a system extension that did not merge:"
    rm -f "$SEC/pam_timebandits.so"
    expect allow kid   acct_mgmt    "the child is not locked out of the machine"
    expect allow grown acct_mgmt    "and neither is anybody else"

    # Everything above isolates our module from pam_unix on purpose. Where the
    # environment can check a real password, do the two cases that matter over
    # the real thing as well.
    install -Dm0755 /stage/libpam_timebandits.so "$SEC/pam_timebandits.so"
    timebanditsd --config /etc/timebandits/daemon.toml >>/tmp/daemon.log 2>&1 &
    dpid=$!
    for _ in $(seq 1 40); do [ -S /run/timebandits/pam.sock ] && break; sleep 0.25; done
    if echo secret | pamtester tb-unix grown authenticate >/dev/null 2>&1; then
        echo "over a real pam_unix:"
        SERVICE=tb-unix expect deny  kid   authenticate "the correct password is refused"
        SERVICE=tb-unix expect allow grown authenticate "the adult is unaffected"
    else
        echo "over a real pam_unix: skipped — this container cannot check a password"
    fi
    kill "$dpid" 2>/dev/null || true

    echo
    if [ "$fail" -gt 0 ]; then
        printf '\033[31m%d checks failed\033[0m\n' "$fail"; exit 1
    fi
    printf '\033[32mevery check passed\033[0m\n'
    exit 0
fi

# --- the part that runs on the host ------------------------------------------

cd "$(dirname "$0")/../.." || exit 1

runtime=""
for r in podman docker; do command -v "$r" >/dev/null && { runtime="$r"; break; }; done
[ -n "$runtime" ] || { echo "needs podman or docker"; exit 1; }

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

# Built inside the same image, so the binaries match its libc rather than the
# host's.
echo "building in $IMAGE"
"$runtime" run --rm -v "$PWD:/src:z" -v "$stage:/out:z" -w /src "$IMAGE" bash -c '
set -e
dnf install -y -q --setopt=install_weak_deps=False \
    cargo rust make gcc pam-devel sqlite-devel >/dev/null
cargo build --release --locked -p tb-daemon -p tb-pam -p tb-cli
cp target/release/timebanditsd target/release/tbctl \
   target/release/libpam_timebandits.so /out/
'

echo "running the PAM checks"
exec "$runtime" run --rm \
    -v "$PWD/tests/pam/integration.sh:/t.sh:ro,z" \
    -v "$stage:/stage:ro,z" \
    "$IMAGE" bash /t.sh --inside
