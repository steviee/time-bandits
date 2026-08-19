# SPDX-FileCopyrightText: 2026 Time Bandits contributors
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The single source of truth for where files go.
#
# Every packaging recipe — PKGBUILD, RPM spec, debian/rules — calls this target
# rather than listing files itself. Three hand-maintained file lists is how a
# new file ends up in the Arch package and missing from the Debian one.

PREFIX      ?= /usr
BINDIR      ?= $(PREFIX)/bin
DATADIR     ?= $(PREFIX)/share
SYSCONFDIR  ?= /etc
UNITDIR     ?= $(PREFIX)/lib/systemd/system

# PAM modules live in a different place on every distribution:
#   Arch          /usr/lib/security
#   Fedora/RHEL   /usr/lib64/security
#   Debian        /usr/lib/<triplet>/security
# Each recipe passes the right one; this default suits a plain lib layout.
PAMDIR      ?= $(PREFIX)/lib/security

CARGO       ?= cargo
CARGO_FLAGS ?= --release --locked
TARGETDIR   ?= target/release
INSTALL     ?= install

VERSION  ?= 0.1.0
DISTNAME  = time-bandits-$(VERSION)

.PHONY: all build install install-daemon install-pam install-config install-docs check clean vendor dist

all: build

build:
	$(CARGO) build $(CARGO_FLAGS) -p tb-daemon -p tb-pam

check:
	$(CARGO) test --workspace --locked

# Distribution packages usually place documentation themselves (%doc, dh_installdocs),
# so install-docs is not part of the default set.
install: install-daemon install-pam install-config

install-daemon:
	$(INSTALL) -Dm0755 $(TARGETDIR)/timebanditsd $(DESTDIR)$(BINDIR)/timebanditsd
	$(INSTALL) -Dm0644 packaging/systemd/timebanditsd.service \
		$(DESTDIR)$(UNITDIR)/timebanditsd.service

install-pam:
	$(INSTALL) -Dm0755 $(TARGETDIR)/libpam_timebandits.so \
		$(DESTDIR)$(PAMDIR)/pam_timebandits.so

# The daemon reads its own defaults, so this file is entirely commented out.
# It exists to document the options in a place administrators will look.
install-config:
	$(INSTALL) -Dm0644 packaging/config/daemon.toml \
		$(DESTDIR)$(SYSCONFDIR)/timebandits/daemon.toml

install-docs:
	$(INSTALL) -Dm0644 README.md $(DESTDIR)$(DATADIR)/doc/time-bandits/README.md
	$(INSTALL) -Dm0644 docs/pam-setup.md $(DESTDIR)$(DATADIR)/doc/time-bandits/pam-setup.md
	$(INSTALL) -Dm0644 docs/architecture.md $(DESTDIR)$(DATADIR)/doc/time-bandits/architecture.md
	$(INSTALL) -Dm0644 docs/threat-model.md $(DESTDIR)$(DATADIR)/doc/time-bandits/threat-model.md

# Produces the vendored-dependency tarball that offline distro builds need.
# cargo-vendor-filterer drops platform-specific crates we never build, which
# keeps the tarball roughly a third of the size and the licence audit shorter.
# cargo-vendor-filterer drops crates for platforms we never build, roughly
# halving the tarball and shortening the licence audit. It is an extra tool, so
# fall back to plain `cargo vendor` when it is not installed rather than making
# every packager install it.
vendor:
	@set -e; \
	if command -v cargo-vendor-filterer >/dev/null 2>&1; then \
		echo "vendoring with cargo-vendor-filterer"; \
		cargo vendor-filterer --platform=x86_64-unknown-linux-gnu \
			--platform=aarch64-unknown-linux-gnu \
			--format=tar.zstd vendor.tar.zst; \
	else \
		echo "cargo-vendor-filterer not found, using plain cargo vendor"; \
		cargo vendor --locked --versioned-dirs vendor >/dev/null; \
		tar --zstd -cf vendor.tar.zst vendor; \
		rm -rf vendor; \
	fi; \
	size=$$(stat -c%s vendor.tar.zst); \
	if [ "$$size" -lt 100000 ]; then \
		echo "vendor.tar.zst is only $$size bytes — vendoring produced nothing usable" >&2; \
		exit 1; \
	fi
	@ls -lh vendor.tar.zst

# The release tarball. Packaging recipes consume this plus vendor.tar.zst,
# so a package built from a git checkout and one built from a release are the
# same thing.
# Works from a git checkout and from an unpacked release alike. The second
# case is not hypothetical: actions/checkout falls back to downloading a
# tarball when the container has no git, leaving no .git directory behind.
dist: vendor
	@if [ -d .git ] && command -v git >/dev/null 2>&1; then \
		echo "packing from git"; \
		git archive --format=tar.gz --prefix=$(DISTNAME)/ -o $(DISTNAME).tar.gz HEAD; \
	else \
		echo "no git checkout, packing the working tree"; \
		tmp=$$(mktemp -d); \
		tar czf $$tmp/$(DISTNAME).tar.gz --transform 's,^\.,$(DISTNAME),' \
			--exclude=./target --exclude=./.git --exclude=./vendor \
			--exclude=./vendor.tar.zst --exclude='./*.tar.gz' \
			--exclude=./debian --exclude=./node_modules .; \
		mv $$tmp/$(DISTNAME).tar.gz $(DISTNAME).tar.gz; \
		rmdir $$tmp; \
	fi
	@ls -lh $(DISTNAME).tar.gz vendor.tar.zst

clean:
	$(CARGO) clean
	rm -f vendor.tar.zst $(DISTNAME).tar.gz
