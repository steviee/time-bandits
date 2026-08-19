# SPDX-FileCopyrightText: 2026 Time Bandits contributors
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Upstream-provided spec, aimed at COPR and at people building from a release
# tarball. A package accepted *into* Fedora would be rewritten by its maintainer
# using the %%cargo_* macros and unbundled crates; see packaging/README.md for
# why this one does not.

%global debug_package %{nil}

Name:           time-bandits
Version:        0.1.0
Release:        1%{?dist}
Summary:        Screen-time management for KDE Plasma households

License:        GPL-3.0-or-later
URL:            https://github.com/steviee/time-bandits
Source0:        %{url}/archive/v%{version}/%{name}-%{version}.tar.gz
# Produced by `make vendor`. Build hosts have no network, so dependencies
# have to travel with the source.
Source1:        vendor.tar.zst

ExclusiveArch:  x86_64 aarch64

# 1.90 is the verified minimum: the workspace builds and its tests pass on it.
BuildRequires:  cargo >= 1.90
BuildRequires:  rust >= 1.90
BuildRequires:  gcc
BuildRequires:  make
BuildRequires:  pam-devel
BuildRequires:  sqlite-devel
BuildRequires:  systemd-rpm-macros
BuildRequires:  zstd

Requires:       pam
Requires:       sqlite-libs
# The daemon is useless without a login manager to enforce against.
Recommends:     systemd

%description
Time Bandits measures how long children spend at the computer and enforces
daily and weekly limits plus allowed time windows.

Enforcement works at two levels: systemd-logind locks or ends a session when
time runs out, and a PAM module refuses both the unlock and the next login.
The PAM half matters because KScreenLocker authenticates through PAM, so
without it a child could simply unlock again with their own password.

It runs standalone on a single machine, or reports to a household server.

%prep
%autosetup -n %{name}-%{version} -p1
tar --zstd -xf %{SOURCE1}
mkdir -p .cargo
cat > .cargo/config.toml <<'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF

%build
export CARGO_NET_OFFLINE=true
export CARGO_HOME=%{_builddir}/cargo-home
%make_build build CARGO_FLAGS="--release --locked --offline"

%check
export CARGO_NET_OFFLINE=true
export CARGO_HOME=%{_builddir}/cargo-home
cargo test --workspace --locked --offline

%install
%make_install \
    PREFIX=%{_prefix} \
    BINDIR=%{_bindir} \
    SYSCONFDIR=%{_sysconfdir} \
    UNITDIR=%{_unitdir} \
    PAMDIR=%{_libdir}/security

%post
%systemd_post timebanditsd.service

%preun
%systemd_preun timebanditsd.service
# Remove our own lines from /etc/pam.d before the module file disappears.
# A `required` line pointing at a missing module makes PAM fail, and then
# nobody can log in — this is the one uninstall step that is not optional.
if [ $1 -eq 0 ]; then
    for f in %{_sysconfdir}/pam.d/*; do
        [ -f "$f" ] || continue
        if grep -qF '# >>> time-bandits >>>' "$f"; then
            sed -i '\|# >>> time-bandits >>>|,\|# <<< time-bandits <<<|d' "$f"
        fi
    done
fi

%postun
%systemd_postun_with_restart timebanditsd.service

%files
%license LICENSES/GPL-3.0-or-later.txt
%doc README.md docs/pam-setup.md docs/architecture.md docs/threat-model.md
%{_bindir}/timebanditsd
%{_libdir}/security/pam_timebandits.so
%{_unitdir}/timebanditsd.service
%dir %{_sysconfdir}/timebandits
%config(noreplace) %{_sysconfdir}/timebandits/daemon.toml

%changelog
* Wed Aug 19 2026 Stephan Eberle <1726811+steviee@users.noreply.github.com> - 0.1.0-1
- Initial package: daemon, PAM module and systemd unit
