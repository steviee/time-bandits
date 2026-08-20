#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Time Bandits contributors
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Compiles the SELinux policy module. Needs checkpolicy and policycoreutils,
# which an image-based system does not have — build it in a container of the
# matching distribution, the way packaging/sysext/build.sh does.
#
#     packaging/selinux/build.sh [output.pp]

set -euo pipefail
cd "$(dirname "$0")" || exit 1

OUT="${1:-$PWD/timebandits.pp}"
command -v checkmodule >/dev/null || { echo "needs checkmodule (checkpolicy)" >&2; exit 1; }
command -v semodule_package >/dev/null || { echo "needs semodule_package" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

checkmodule -M -m -o "$tmp/timebandits.mod" timebandits.te
# -f, or the file contexts never reach the policy store and the runtime
# directory keeps the generic var_run_t label the rules do not cover.
semodule_package -o "$OUT" -m "$tmp/timebandits.mod" -f timebandits.fc
echo "wrote $OUT"
