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
USERUNITDIR ?= $(PREFIX)/lib/systemd/user
SYSUSERSDIR ?= $(PREFIX)/lib/sysusers.d

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

.PHONY: all build build-plugin po-extract po-update install install-daemon install-pam install-config install-docs install-plasma check clean vendor dist

all: build

build:
	$(CARGO) build $(CARGO_FLAGS) -p tb-daemon -p tb-pam -p tb-cli -p tb-agent

# Rebuilds the message template from the QML. --no-wrap on purpose: a long
# string split across lines is a string tools quietly fail to match.
po-extract:
	xgettext --from-code=UTF-8 --language=JavaScript --no-wrap \
		--keyword=i18n:1 --keyword=i18nc:1c,2 --keyword=i18np:1,2 --keyword=i18ncp:1c,2,3 \
		--package-name="Time Bandits" --package-version=$(VERSION) \
		--msgid-bugs-address="https://github.com/steviee/time-bandits/issues" \
		--copyright-holder="Time Bandits contributors" --add-comments=/// \
		-o plasmoid/po/$(PO_DOMAIN).pot \
		plasmoid/$(PLASMOID_ID)/contents/ui/*.qml

# Folds new and changed strings into the existing translations, keeping what is
# already translated.
po-update: po-extract
	@for lang in $(LANGUAGES); do \
		msgmerge --no-wrap --update --backup=none \
			plasmoid/po/$$lang.po plasmoid/po/$(PO_DOMAIN).pot; \
		msgfmt --check --statistics -o /dev/null plasmoid/po/$$lang.po; \
	done

# The one component CMake builds: Plasma 6 gives QML no way to speak D-Bus, so
# the widget needs a compiled plugin. Qt only — no KDE Frameworks, no ECM.
build-plugin:
	cmake -S plasmoid/plugin -B plasmoid/plugin/build \
		-DCMAKE_BUILD_TYPE=Release -DQML_INSTALL_DIR=$(QMLDIR)
	cmake --build plasmoid/plugin/build

check:
	$(CARGO) test --workspace --locked

# The Plasma front end is a separate package: the enforcing half depends on no
# desktop, and dragging KDE into it would be a lie. See packaging/README.md.
PLASMOID_ID  = org.timebandits.screentime
PLASMOIDDIR ?= $(DATADIR)/plasma/plasmoids
QMLDIR      ?= $(shell qmake6 -query QT_INSTALL_QML 2>/dev/null || echo $(PREFIX)/lib/qt6/qml)

# Distribution packages usually place documentation themselves (%doc, dh_installdocs),
# so install-docs is not part of the default set.
install: install-daemon install-pam install-config

install-daemon:
	$(INSTALL) -Dm0755 $(TARGETDIR)/timebanditsd $(DESTDIR)$(BINDIR)/timebanditsd
	$(INSTALL) -Dm0755 $(TARGETDIR)/tbctl $(DESTDIR)$(BINDIR)/tbctl
	$(INSTALL) -Dm0755 $(TARGETDIR)/timebandits-agent $(DESTDIR)$(BINDIR)/timebandits-agent
	$(INSTALL) -Dm0644 packaging/systemd/timebanditsd.service \
		$(DESTDIR)$(UNITDIR)/timebanditsd.service
	$(INSTALL) -Dm0644 packaging/systemd/timebandits-agent.service \
		$(DESTDIR)$(USERUNITDIR)/timebandits-agent.service
	$(INSTALL) -Dm0644 packaging/sysusers/timebandits.conf \
		$(DESTDIR)$(SYSUSERSDIR)/timebandits.conf

install-pam:
	$(INSTALL) -Dm0755 $(TARGETDIR)/libpam_timebandits.so \
		$(DESTDIR)$(PAMDIR)/pam_timebandits.so

# The daemon reads its own defaults, so this file is entirely commented out.
# It exists to document the options in a place administrators will look.
install-config:
	$(INSTALL) -Dm0644 packaging/config/daemon.toml \
		$(DESTDIR)$(SYSCONFDIR)/timebandits/daemon.toml
	# One TOML file per child lands here. Shipped empty: which children exist
	# is a decision for the household, not for the package.
	$(INSTALL) -d -m0755 $(DESTDIR)$(SYSCONFDIR)/timebandits/policy.d

PO_DOMAIN       = plasma_applet_$(PLASMOID_ID)
LOCALEDIR      ?= $(DATADIR)/locale
LANGUAGES       = de

KWIN_SCRIPT_ID  = org.timebandits.focus
KWINSCRIPTDIR  ?= $(DATADIR)/kwin/scripts

install-plasma: build-plugin
	DESTDIR=$(DESTDIR) cmake --install plasmoid/plugin/build
	$(INSTALL) -d $(DESTDIR)$(PLASMOIDDIR)/$(PLASMOID_ID)
	cp -r plasmoid/$(PLASMOID_ID)/metadata.json plasmoid/$(PLASMOID_ID)/contents \
		$(DESTDIR)$(PLASMOIDDIR)/$(PLASMOID_ID)/
	@for lang in $(LANGUAGES); do \
		$(INSTALL) -d $(DESTDIR)$(LOCALEDIR)/$$lang/LC_MESSAGES; \
		msgfmt -o $(DESTDIR)$(LOCALEDIR)/$$lang/LC_MESSAGES/$(PO_DOMAIN).mo \
			plasmoid/po/$$lang.po; \
	done
	$(INSTALL) -d $(DESTDIR)$(KWINSCRIPTDIR)/$(KWIN_SCRIPT_ID)
	cp -r kwin-script/$(KWIN_SCRIPT_ID)/metadata.json kwin-script/$(KWIN_SCRIPT_ID)/contents \
		$(DESTDIR)$(KWINSCRIPTDIR)/$(KWIN_SCRIPT_ID)/

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
	rm -rf plasmoid/plugin/build
	rm -f vendor.tar.zst $(DISTNAME).tar.gz

# --- system extension (image-based systems: Bazzite, Kinoite, bootc) --------

.PHONY: sysext
sysext:
	packaging/sysext/build.sh $(CURDIR)/timebandits.raw
