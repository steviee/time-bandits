# SPDX-FileCopyrightText: 2026 Time Bandits contributors
# SPDX-License-Identifier: GPL-3.0-or-later

# Alle Aufgaben laufen über `mise exec` bzw. die von mise bereitgestellten Shims.

default:
    @just --list

# Vollständige Prüfung, wie sie auch die CI fährt.
check: fmt-check lint test

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

# Release-Binaries bauen.
build:
    cargo build --workspace --release

# PAM-Modul isoliert bauen und die entstandene Bibliothek zeigen.
pam:
    cargo build -p tb-pam --release
    @ls -lh target/release/libpam_timebandits.so

# Abhängigkeiten und Lizenzen prüfen.
audit:
    cargo deny check
