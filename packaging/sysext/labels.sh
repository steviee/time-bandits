# shellcheck shell=bash
# SPDX-FileCopyrightText: 2026 Time Bandits contributors
# SPDX-License-Identifier: GPL-3.0-or-later
#
# SELinux labels for a system extension, and why they are checked so hard.
#
# In an overlayfs merge, a directory present in both layers takes the *upper*
# layer's label. Our image contains /usr/bin and /usr/lib64/security, so if
# those carry the wrong context, they carry it for the whole merged system —
# and a wrongly labelled /usr/lib64/security means no process can load any PAM
# module at all. Not ours: any. No sudo, no ssh, no login.
#
# That is not a hypothetical. An image staged through a container bind mount
# came out labelled container_file_t, `setfiles` reported success without
# fixing it, and merging it took a working machine down to no logins at all.
# Hence: the label check is not advisory, and the build refuses without it.

context_of() {
    getfattr -n security.selinux --only-values --absolute-names "$1" 2>/dev/null \
        | tr -d '\0'
}

# The type field, which is what the targeted policy actually decides on. The
# user field differs harmlessly between a file `setfiles` labelled and one RPM
# installed (unconfined_u against system_u), and comparing it produces alarms
# about images that are perfectly fine.
type_of() {
    context_of "$1" | cut -d: -f3
}

# Every path in the tree that also exists on the host must carry the host's
# context. This is the invariant the overlay actually depends on, and it needs
# no table of expected types to check.
check_labels_against_host() {
    local root="$1" bad=0 checked=0 rel host want got
    while IFS= read -r rel; do
        host="/${rel#./}"
        [ -e "$host" ] || continue
        want="$(type_of "$host")"
        got="$(type_of "$root/${rel#./}")"
        [ -n "$want" ] || continue
        checked=$((checked + 1))
        if [ "$want" != "$got" ]; then
            printf '  %-44s image=%s host=%s\n' "$host" "${got:-none}" "$want" >&2
            bad=$((bad + 1))
        fi
    done < <(cd "$root" && find . -mindepth 1)

    if [ "$bad" -gt 0 ]; then
        echo >&2
        echo "$bad path(s) would change label on merge, out of $checked shared with the host." >&2
        echo "Merging this image would relabel those paths for the whole system." >&2
        return 1
    fi
    echo "labels: $checked shared paths match the host"
}

# Our own files do not exist on the host, so there is nothing to compare them
# to there. Ask the policy instead: `matchpathcon` gives the context a path
# should have, which is exactly what `setfiles` claims to have applied. The
# comparison is therefore a direct test of whether it actually did — the thing
# that silently failed and cost a machine its logins.
check_labels_against_policy() {
    local root="$1" bad=0 checked=0 rel want got
    command -v matchpathcon >/dev/null || {
        echo "labels: no matchpathcon here, skipping the policy check"
        return 0
    }
    while IFS= read -r rel; do
        want="$(matchpathcon "/${rel#./}" 2>/dev/null | awk '{print $NF}' | cut -d: -f3)"
        [ -n "$want" ] && [ "$want" != "<<none>>" ] || continue
        got="$(type_of "$root/${rel#./}")"
        checked=$((checked + 1))
        if [ "$want" != "$got" ]; then
            printf '  %-44s is %s; the policy says %s\n' \
                "/${rel#./}" "${got:-none}" "$want" >&2
            bad=$((bad + 1))
        fi
    done < <(cd "$root" && find . -mindepth 1)

    if [ "$bad" -gt 0 ]; then
        echo >&2
        echo "$bad path(s) are not labelled the way this host's policy says." >&2
        return 1
    fi
    echo "labels: $checked paths match the policy"
}
