#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Time Bandits contributors
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Builds a systemd system extension image.
#
#     packaging/sysext/build.sh                    # build here
#     packaging/sysext/build.sh --container        # build in a Fedora container
#     packaging/sysext/build.sh --stage DIR        # use a tree somebody else staged
#     packaging/sysext/build.sh -o /tmp/tb.raw
#
# The two halves happen in different places on purpose. Staging needs a Rust and
# Qt toolchain, which an image-based system deliberately does not have — hence
# --container. Labelling and packing have to happen on the target host, because
# the SELinux contexts have to come from *its* policy.
#
# See packaging/sysext/README.md for what is in the image and what cannot be.

set -euo pipefail
cd "$(dirname "$0")/../.." || exit 1

NAME=timebandits
OUT="$PWD/${NAME}.raw"
STAGE=""
MODE=here
IMAGE="${TB_SYSEXT_IMAGE:-registry.fedoraproject.org/fedora:44}"

while [ $# -gt 0 ]; do
    case "$1" in
        -o|--output)   OUT="$2"; shift 2 ;;
        --stage)       STAGE="$2"; MODE=staged; shift 2 ;;
        --container)   MODE=container; shift ;;
        -h|--help)     sed -n '3,18p' "$0" | sed 's/^# \?//'; exit 0 ;;
        *)             echo "unknown option: $1" >&2; exit 1 ;;
    esac
done

command -v mksquashfs >/dev/null || { echo "needs mksquashfs (squashfs-tools)" >&2; exit 1; }

# --- stage the /usr tree -----------------------------------------------------

stage_here() {
    make build >/dev/null
    # install-plasma too: a client package without the widget is not a client.
    make install install-plasma DESTDIR="$1" \
        PREFIX=/usr \
        PAMDIR=/usr/lib64/security \
        UNITDIR=/usr/lib/systemd/system \
        USERUNITDIR=/usr/lib/systemd/user \
        SYSUSERSDIR=/usr/lib/sysusers.d >/dev/null
}

stage_in_container() {
    local runtime=""
    for r in podman docker; do command -v "$r" >/dev/null && { runtime="$r"; break; }; done
    [ -n "$runtime" ] || { echo "needs podman or docker for --container" >&2; exit 1; }
    echo "staging in $IMAGE"
    # The result comes back as a tar stream rather than through a bind mount.
    # A `:z` mount relabels the host directory to container_file_t, and that is
    # exactly where an image once got labels that took a machine down to no
    # logins at all. Nothing on the host is handed to the container to write.
    "$runtime" run --rm -i -v "$PWD:/src:ro,z" -w /src "$IMAGE" bash -c '
set -e
dnf install -y -q --setopt=install_weak_deps=False \
    cargo rust make gcc cmake gettext pam-devel sqlite-devel \
    qt6-qtbase-devel qt6-qtdeclarative-devel >&2
# /src is read-only, so build in a copy.
cp -a /src /build && cd /build
make build >&2
make install install-plasma DESTDIR=/stage \
    PREFIX=/usr \
    PAMDIR=/usr/lib64/security \
    UNITDIR=/usr/lib/systemd/system \
    USERUNITDIR=/usr/lib/systemd/user \
    SYSUSERSDIR=/usr/lib/sysusers.d >&2
tar -C /stage --xattrs --no-selinux -cf - .
' | tar -C "$1" -xf -
}

work=""
if [ "$MODE" = staged ]; then
    [ -d "$STAGE/usr" ] || { echo "$STAGE has no usr/ in it" >&2; exit 1; }
    work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
    cp -a "$STAGE/." "$work/"
else
    work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
    case "$MODE" in
        here)      stage_here "$work" ;;
        container) stage_in_container "$work" ;;
    esac
fi

# /etc is never merged by a system extension, so anything staged there is not
# ours to ship. `tbctl sysext install` writes those parts directly.
rm -rf "${work:?}/etc"

[ -x "$work/usr/bin/timebanditsd" ] || { echo "no daemon in the staged tree" >&2; exit 1; }
[ -f "$work/usr/lib64/security/pam_timebandits.so" ] \
    || { echo "no PAM module in the staged tree" >&2; exit 1; }

install -Dm0644 packaging/sysext/extension-release."$NAME" \
    "$work/usr/lib/extension-release.d/extension-release.$NAME"

# --- label and pack ----------------------------------------------------------

# The labels have to be in the image. A mislabelled pam_timebandits.so cannot be
# loaded by sddm, and because the module is fail-safe, logins keep working while
# enforcement is quietly absent. `setfiles -r` applies the contexts the files
# would have at their real paths.
policy=/etc/selinux/targeted/contexts/files/file_contexts
if [ -f "$policy" ]; then
    [ "$(id -u)" -eq 0 ] || {
        echo "refusing to build an unlabelled image on an SELinux system." >&2
        echo "Re-run with sudo: no PAM module would load without labels." >&2
        exit 1
    }
    setfiles -r "$work" "$policy" "$work"

    # `setfiles` returning 0 is not evidence that it did anything. It reported
    # success on the image that took a machine down to no logins at all, so the
    # labels are read back and checked rather than assumed.
    # shellcheck source=packaging/sysext/labels.sh
    . packaging/sysext/labels.sh
    ok=1
    check_labels_against_host "$work" || ok=0
    check_labels_against_policy "$work" || ok=0
    [ "$ok" = 1 ] || {
        echo >&2
        echo "refusing to write this image." >&2
        exit 1
    }
else
    echo "no SELinux policy here; the image carries no labels"
fi

rm -f "$OUT"
mksquashfs "$work" "$OUT" -all-root -xattrs -noappend -quiet -no-progress
echo "wrote $OUT ($(du -h "$OUT" | cut -f1))"
