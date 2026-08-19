// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Stabile Anwendungs-Kennungen aus uneinheitlichen Quellen.
//!
//! Dieselbe Anwendung erscheint je nach Quelle anders:
//!
//! | Quelle | Firefox als Flatpak |
//! |---|---|
//! | KWin `desktopFileName` | `org.mozilla.firefox` |
//! | KWin `resourceClass`   | `firefox` |
//! | systemd-Scope          | `app-flatpak-org.mozilla.firefox-2891.scope` |
//!
//! Ohne Vereinheitlichung stünde dieselbe App dreimal im Bericht. Diese Datei
//! normalisiert alle drei auf `org.mozilla.firefox`.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Woher eine Kennung stammt. Bestimmt, welcher Beobachtung bei Widerspruch
/// geglaubt wird — und macht in Berichten sichtbar, wie verlässlich ein Eintrag ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppIdSource {
    /// Kein Fokus zuzuordnen (Agent tot, Skript deaktiviert). Schwächste Quelle.
    Unknown,
    /// Aus dem systemd-Scope des Prozesses. Verfügbar auch ohne Sitzungs-Agent.
    SystemdScope,
    /// Aus `resourceClass` des Fensters. Grob, aber immer vorhanden.
    WindowClass,
    /// Aus `desktopFileName` des Fensters. Genaueste Quelle.
    DesktopFile,
}

/// Eine normalisierte Anwendungs-Kennung.
///
/// Immer kleingeschrieben und ohne `.desktop`-Endung, damit Vergleiche und
/// Datenbank-Schlüssel eindeutig sind.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AppId(String);

impl AppId {
    /// Platzhalter für nicht zuordenbare Zeit. Taucht in Berichten als eigener
    /// Posten auf — verschwiegene Zeit wäre schlimmer als sichtbar unbekannte.
    pub const UNKNOWN: &'static str = "unknown";

    #[must_use]
    pub fn unknown() -> Self {
        Self(Self::UNKNOWN.to_owned())
    }

    /// Normalisiert beliebigen Rohtext zu einer Kennung.
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
        f.write_str(&self.0)
    }
}

/// Eine Beobachtung: welche App, aus welcher Quelle.
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

    /// Behält die verlässlichere von zwei Beobachtungen.
    #[must_use]
    pub fn best_of(self, other: Self) -> Self {
        if other.source > self.source {
            other
        } else {
            self
        }
    }
}

/// Macht systemd-Unit-Escaping rückgängig (`\x2d` → `-`).
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

/// Bekannte Starter-Präfixe, die systemd zwischen `app-` und die eigentliche
/// Kennung schiebt. Die Groß-/Kleinschreibung ist je nach Plasma-Version anders.
const LAUNCHER_PREFIXES: &[&str] = &["flatpak-", "snap-", "gnome-", "kde-", "wayland-", "glib-"];

/// Zieht die Anwendungs-Kennung aus einem systemd-Unit-Namen.
///
/// Erkennt unter anderem:
/// * `app-org.kde.konsole-9d0a.scope`
/// * `app-flatpak-org.mozilla.firefox-2891.scope`
/// * `app-KDE-org.kde.dolphin-1234.scope`
/// * `app-org.kde.dolphin@a1b2c3.service`
///
/// Gibt `None` zurück, wenn die Unit gar keine Anwendung beschreibt (etwa
/// `user@1000.service` oder `session-2.scope`) — dann ist Schweigen richtig.
#[must_use]
pub fn app_id_from_unit(unit: &str) -> Option<AppId> {
    let unit = unescape_unit(unit.trim());
    let unit = unit
        .strip_suffix(".scope")
        .or_else(|| unit.strip_suffix(".service"))?;
    let mut rest = unit.strip_prefix("app-")?;

    // Starter-Präfix entfernen, unabhängig von Groß-/Kleinschreibung.
    let lower = rest.to_ascii_lowercase();
    for p in LAUNCHER_PREFIXES {
        if lower.starts_with(p) {
            rest = &rest[p.len()..];
            break;
        }
    }

    // Instanz-Kennung abschneiden: `@<token>` oder ein abschließendes
    // `-<hex/zahl>`. Ein Bindestrich mitten im Namen (`org.kde.k-menu`) bleibt.
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

/// Baut die beste Beobachtung aus dem, was KWin über ein Fenster liefert.
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
    fn normalisiert_desktop_dateinamen() {
        assert_eq!(
            AppId::new("org.kde.Konsole.desktop").as_str(),
            "org.kde.konsole"
        );
        assert_eq!(AppId::new("org.kde.konsole").as_str(), "org.kde.konsole");
        assert_eq!(AppId::new("  Firefox  ").as_str(), "firefox");
    }

    #[test]
    fn leerer_name_wird_unknown() {
        assert!(AppId::new("").is_unknown());
        assert!(AppId::new("   ").is_unknown());
        // Reine Sonderzeichen ergeben keine brauchbare Kennung.
        assert!(AppId::new("///").is_unknown());
    }

    #[test]
    fn liest_kde_scope() {
        assert_eq!(
            app_id_from_unit("app-org.kde.konsole-9d0a.scope")
                .unwrap()
                .as_str(),
            "org.kde.konsole"
        );
    }

    #[test]
    fn liest_flatpak_scope() {
        assert_eq!(
            app_id_from_unit("app-flatpak-org.mozilla.firefox-2891.scope")
                .unwrap()
                .as_str(),
            "org.mozilla.firefox"
        );
    }

    #[test]
    fn liest_scope_mit_starter_praefix_und_instanz() {
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
    fn macht_systemd_escaping_rueckgaengig() {
        // systemd kodiert `-` im Anwendungsnamen als `\x2d`.
        assert_eq!(
            app_id_from_unit(r"app-my\x2dgame-4f2a.scope")
                .unwrap()
                .as_str(),
            "my-game"
        );
    }

    #[test]
    fn bindestrich_im_namen_bleibt_erhalten() {
        // `menu` ist kein Hex-Suffix, darf also nicht abgeschnitten werden.
        assert_eq!(
            app_id_from_unit("app-org.kde.k-menu.scope")
                .unwrap()
                .as_str(),
            "org.kde.k-menu"
        );
    }

    #[test]
    fn ignoriert_units_die_keine_apps_sind() {
        assert!(app_id_from_unit("user@1000.service").is_none());
        assert!(app_id_from_unit("session-2.scope").is_none());
        assert!(app_id_from_unit("plasma-plasmashell.service").is_none());
        assert!(app_id_from_unit("app-.scope").is_none());
        assert!(app_id_from_unit("nicht-mal-eine-unit").is_none());
    }

    #[test]
    fn fensterbeobachtung_bevorzugt_desktop_datei() {
        let o = observe_window(Some("org.mozilla.firefox"), Some("firefox"));
        assert_eq!(o.source, AppIdSource::DesktopFile);
        assert_eq!(o.app.as_str(), "org.mozilla.firefox");
    }

    #[test]
    fn fensterbeobachtung_faellt_auf_fensterklasse_zurueck() {
        let o = observe_window(None, Some("Firefox"));
        assert_eq!(o.source, AppIdSource::WindowClass);
        assert_eq!(o.app.as_str(), "firefox");
        // Leerer desktopFileName kommt bei manchen Anwendungen tatsächlich vor.
        let o = observe_window(Some(""), Some("steam"));
        assert_eq!(o.source, AppIdSource::WindowClass);
    }

    #[test]
    fn ohne_jede_angabe_bleibt_unknown() {
        let o = observe_window(None, None);
        assert_eq!(o.source, AppIdSource::Unknown);
        assert!(o.app.is_unknown());
    }

    #[test]
    fn verlaesslichere_quelle_gewinnt() {
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
        // Unknown verdrängt nie eine echte Beobachtung.
        assert_eq!(desktop.clone().best_of(AppObservation::unknown()), desktop);
    }
}
