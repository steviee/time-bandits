# SPDX-FileCopyrightText: 2026 Time Bandits contributors
# SPDX-License-Identifier: GPL-3.0-or-later

# Every recipe runs through mise's shims.

default:
    @just --list

# Everything CI checks, in the order CI checks it. Run this before pushing —
# it exists because "the tests pass" is not the same as "the pipeline is green",
# and licensing drifted red for eight commits while the tests stayed green.
check: fmt-check lint test licensing guards

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

# The VM walkthrough's safety guards. They are shell, so they get their own
# tests: the check that refuses to run on a machine somebody depends on was
# wrong the first time it ran.
guards:
    ./tests/vm/lib_test.sh

# Every file needs a copyright and a licence, including the ones that cannot
# carry a comment header — those are declared in REUSE.toml.
licensing:
    @command -v reuse >/dev/null 2>&1 && reuse lint || python3 -m reuse lint

# Release binaries, through the Makefile so the crate list stays in one place.
build:
    make build

# The three packaging recipes install from the same Makefile. This proves a
# staged install actually contains every binary, which is what the Arch
# package silently stopped doing.
staged-install:
    #!/usr/bin/env bash
    set -euo pipefail
    dest=$(mktemp -d)
    trap 'rm -rf "$dest"' EXIT
    make build
    make install DESTDIR="$dest" PREFIX=/usr
    for f in usr/bin/timebanditsd usr/bin/tbctl usr/bin/timebandits-agent \
             etc/timebandits/daemon.toml etc/timebandits/policy.d; do
        test -e "$dest/$f" || { echo "missing: $f"; exit 1; }
    done
    echo "staged install is complete"

# Build the PAM module on its own and show what came out.
pam:
    cargo build -p tb-pam --release
    @ls -lh target/release/libpam_timebandits.so

# Dependency and licence audit.
audit:
    cargo deny check
