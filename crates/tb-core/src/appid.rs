// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Stable application identifiers from inconsistent sources.
//!
//! The same application looks different depending on where it is observed:
//!
//! | source | Firefox installed as a Flatpak |
//! |---|---|
//! | `KWin` `desktopFileName` | `org.mozilla.firefox` |
//! | `KWin` `resourceClass`   | `firefox` |
//! | systemd scope            | `app-flatpak-org.mozilla.firefox-2891.scope` |
//!
//! Without normalization one application would appear three times in a report.
//! This module maps all three onto `org.mozilla.firefox`.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Where an identifier came from. Decides which observation wins when two
/// disagree, and makes the confidence of a report entry visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppIdSource {
    /// No focus could be attributed (agent dead, script disabled). Weakest.
    Unknown,
    /// From the process's systemd scope. Available without a session agent.
    SystemdScope,
    /// From the window's `resourceClass`. Coarse, but always present.
    WindowClass,
    /// From the window's `desktopFileName`. The most precise source.
    DesktopFile,
}

/// A normalized application identifier.
///
/// Always lowercase and without the `.desktop` suffix so comparisons and
/// database keys are unambiguous.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AppId(String);

impl AppId {
    /// Placeholder for time that could not be attributed. It shows up as its own
    /// line in reports — visibly unknown time is better than quietly dropped time.
    pub const UNKNOWN: &'static str = "unknown";

    #[must_use]
    pub fn unknown() -> Self {
        Self(Self::UNKNOWN.to_owned())
    }

    /// Normalizes arbitrary raw text into an identifier.
    #[must_use]
    pub fn new(raw: &str) -> Self {
        let s = raw.trim().trim_end_matches(".desktop").to_ascii_lowercase();
        let s: String = s
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
            .collect();
        if s.is_empty() {
            Self::unknown()
        } else {
            Self(s)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_unknown(&self) -> bool {
        self.0 == Self::UNKNOWN
    }
}

impl fmt::Display for AppId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `pad` rather than `write_str`, so column widths are honoured.
        f.pad(&self.0)
    }
}

/// One observation: which application, from which source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppObservation {
    pub app: AppId,
    pub source: AppIdSource,
}

impl AppObservation {
    #[must_use]
    pub fn unknown() -> Self {
        Self {
            app: AppId::unknown(),
            source: AppIdSource::Unknown,
        }
    }

    /// Keeps the more trustworthy of two observations.
    #[must_use]
    pub fn best_of(self, other: Self) -> Self {
        if other.source > self.source {
            other
        } else {
            self
        }
    }
}

/// Reverses systemd unit-name escaping (`\x2d` → `-`).
fn unescape_unit(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let bytes = name.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() && bytes[i + 1] == b'x' {
            let hex = &name[i + 2..i + 4];
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b as char);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Launcher prefixes systemd inserts between `app-` and the identifier proper.
/// The casing varies between Plasma versions.
const LAUNCHER_PREFIXES: &[&str] = &["flatpak-", "snap-", "gnome-", "kde-", "wayland-", "glib-"];

/// Extracts the application identifier from a systemd unit name.
///
/// Recognizes, among others:
/// * `app-org.kde.konsole-9d0a.scope`
/// * `app-flatpak-org.mozilla.firefox-2891.scope`
/// * `app-KDE-org.kde.dolphin-1234.scope`
/// * `app-org.kde.dolphin@a1b2c3.service`
///
/// Returns `None` when the unit does not describe an application at all (such as
/// `user@1000.service` or `session-2.scope`) — staying silent is correct there.
#[must_use]
pub fn app_id_from_unit(unit: &str) -> Option<AppId> {
    let unit = unescape_unit(unit.trim());
    let unit = unit
        .strip_suffix(".scope")
        .or_else(|| unit.strip_suffix(".service"))?;
    let mut rest = unit.strip_prefix("app-")?;

    // Strip the launcher prefix, case-insensitively.
    let lower = rest.to_ascii_lowercase();
    for p in LAUNCHER_PREFIXES {
        if lower.starts_with(p) {
            rest = &rest[p.len()..];
            break;
        }
    }

    // Cut the instance token: `@<token>` or a trailing `-<hex>`. A hyphen in the
    // middle of a real name (`org.kde.k-menu`) survives.
    let rest = rest.split('@').next().unwrap_or(rest);
    let rest = match rest.rsplit_once('-') {
        Some((head, tail))
            if !head.is_empty()
                && !tail.is_empty()
                && tail.chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            head
        }
        _ => rest,
    };

    let id = AppId::new(rest);
    if id.is_unknown() { None } else { Some(id) }
}

/// Builds the best observation from what `KWin` reports about a window.
#[must_use]
pub fn observe_window(desktop_file: Option<&str>, resource_class: Option<&str>) -> AppObservation {
    if let Some(df) = desktop_file.map(str::trim).filter(|s| !s.is_empty()) {
        let app = AppId::new(df);
        if !app.is_unknown() {
            return AppObservation {
                app,
                source: AppIdSource::DesktopFile,
            };
        }
    }
    if let Some(rc) = resource_class.map(str::trim).filter(|s| !s.is_empty()) {
        let app = AppId::new(rc);
        if !app.is_unknown() {
            return AppObservation {
                app,
                source: AppIdSource::WindowClass,
            };
        }
    }
    AppObservation::unknown()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_desktop_file_names() {
        assert_eq!(
            AppId::new("org.kde.Konsole.desktop").as_str(),
            "org.kde.konsole"
        );
        assert_eq!(AppId::new("org.kde.konsole").as_str(), "org.kde.konsole");
        assert_eq!(AppId::new("  Firefox  ").as_str(), "firefox");
    }

    #[test]
    fn an_empty_name_becomes_unknown() {
        assert!(AppId::new("").is_unknown());
        assert!(AppId::new("   ").is_unknown());
        // Punctuation alone yields no usable identifier.
        assert!(AppId::new("///").is_unknown());
    }

    #[test]
    fn reads_a_kde_scope() {
        assert_eq!(
            app_id_from_unit("app-org.kde.konsole-9d0a.scope")
                .unwrap()
                .as_str(),
            "org.kde.konsole"
        );
    }

    #[test]
    fn reads_a_flatpak_scope() {
        assert_eq!(
            app_id_from_unit("app-flatpak-org.mozilla.firefox-2891.scope")
                .unwrap()
                .as_str(),
            "org.mozilla.firefox"
        );
    }

    #[test]
    fn reads_a_scope_with_launcher_prefix_and_instance() {
        assert_eq!(
            app_id_from_unit("app-KDE-org.kde.dolphin-1234.scope")
                .unwrap()
                .as_str(),
            "org.kde.dolphin"
        );
        assert_eq!(
            app_id_from_unit("app-org.kde.dolphin@a1b2c3.service")
                .unwrap()
                .as_str(),
            "org.kde.dolphin"
        );
    }

    #[test]
    fn reverses_systemd_escaping() {
        // systemd encodes a `-` inside the application name as `\x2d`.
        assert_eq!(
            app_id_from_unit(r"app-my\x2dgame-4f2a.scope")
                .unwrap()
                .as_str(),
            "my-game"
        );
    }

    #[test]
    fn a_hyphen_inside_the_name_survives() {
        // `menu` is not a hex suffix and must not be cut off.
        assert_eq!(
            app_id_from_unit("app-org.kde.k-menu.scope")
                .unwrap()
                .as_str(),
            "org.kde.k-menu"
        );
    }

    #[test]
    fn ignores_units_that_are_not_applications() {
        assert!(app_id_from_unit("user@1000.service").is_none());
        assert!(app_id_from_unit("session-2.scope").is_none());
        assert!(app_id_from_unit("plasma-plasmashell.service").is_none());
        assert!(app_id_from_unit("app-.scope").is_none());
        assert!(app_id_from_unit("not-even-a-unit").is_none());
    }

    #[test]
    fn window_observation_prefers_the_desktop_file() {
        let o = observe_window(Some("org.mozilla.firefox"), Some("firefox"));
        assert_eq!(o.source, AppIdSource::DesktopFile);
        assert_eq!(o.app.as_str(), "org.mozilla.firefox");
    }

    #[test]
    fn window_observation_falls_back_to_the_window_class() {
        let o = observe_window(None, Some("Firefox"));
        assert_eq!(o.source, AppIdSource::WindowClass);
        assert_eq!(o.app.as_str(), "firefox");
        // An empty desktopFileName really does occur for some applications.
        let o = observe_window(Some(""), Some("steam"));
        assert_eq!(o.source, AppIdSource::WindowClass);
    }

    #[test]
    fn with_nothing_reported_it_stays_unknown() {
        let o = observe_window(None, None);
        assert_eq!(o.source, AppIdSource::Unknown);
        assert!(o.app.is_unknown());
    }

    #[test]
    fn the_more_trustworthy_source_wins() {
        let scope = AppObservation {
            app: AppId::new("firefox"),
            source: AppIdSource::SystemdScope,
        };
        let desktop = AppObservation {
            app: AppId::new("org.mozilla.firefox"),
            source: AppIdSource::DesktopFile,
        };
        assert_eq!(scope.clone().best_of(desktop.clone()), desktop);
        assert_eq!(desktop.clone().best_of(scope), desktop);
        // Unknown never displaces a real observation.
        assert_eq!(desktop.clone().best_of(AppObservation::unknown()), desktop);
    }
}
