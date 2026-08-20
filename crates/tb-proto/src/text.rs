// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! What the child is told, and in which language.
//!
//! The daemon does not write these sentences. It runs as a systemd service,
//! usually with no `LANG` at all and certainly not the child's — so a message
//! composed there would arrive in the wrong language on a German desktop, or in
//! no language in particular.
//!
//! The split is between facts and prose. The daemon sends facts: *why* access
//! was refused, and the wall-clock time it returns, in digits, because only the
//! daemon knows the policy's time zone. Each front end turns those into a
//! sentence in the locale it can see — the lock screen from the login
//! environment, the agent and the widget from the child's session.
//!
//! The table lives here rather than in each consumer so the lock screen and the
//! widget cannot drift into saying different things about the same refusal.

use serde::{Deserialize, Serialize};

/// Why access was refused. Mirrors `tb_core::engine::DenyReason` deliberately:
/// keeping this crate free of the domain model is what lets the PAM module
/// depend on it without pulling in a time-zone database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    DailyQuota,
    WeeklyQuota,
    OutsideWindow,
    /// The daemon could not be reached and the fallback refused.
    ServiceUnavailable,
}

/// When access returns, as facts rather than prose.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RetryAt {
    /// Wall-clock time in the policy's zone, `HH:MM`. Digits are the same in
    /// every language the project targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock: Option<String>,
    /// Whether that time falls on a later day, so the sentence can say so.
    #[serde(default)]
    pub not_today: bool,
    /// English weekday name when it is further out than tomorrow; the consumer
    /// translates it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weekday: Option<String>,
}

/// The languages shipped at launch. Anything else falls back to English.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Locale {
    #[default]
    English,
    German,
}

impl RetryAt {
    /// Nothing to say about when access returns.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clock.is_none() && !self.not_today && self.weekday.is_none()
    }
}

impl Locale {
    /// Reads the locale from the environment the caller is running in.
    ///
    /// `LC_ALL` beats `LC_MESSAGES` beats `LANG`, as POSIX specifies. In the
    /// PAM module this is the login process's environment, which on a desktop
    /// unlock is the child's session.
    #[must_use]
    pub fn from_env() -> Self {
        for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(value) = std::env::var(key)
                && !value.is_empty()
            {
                return Self::from_tag(&value);
            }
        }
        Self::English
    }

    /// Parses a locale tag such as `de_DE.UTF-8`, `de-AT` or `C`.
    #[must_use]
    pub fn from_tag(tag: &str) -> Self {
        let language = tag
            .split(['_', '-', '.', '@'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match language.as_str() {
            "de" => Self::German,
            _ => Self::English,
        }
    }

    fn weekday(self, english: &str) -> String {
        if self == Self::English {
            return english.to_owned();
        }
        match english {
            "Monday" => "Montag",
            "Tuesday" => "Dienstag",
            "Wednesday" => "Mittwoch",
            "Thursday" => "Donnerstag",
            "Friday" => "Freitag",
            "Saturday" => "Samstag",
            "Sunday" => "Sonntag",
            other => other,
        }
        .to_owned()
    }
}

/// The sentence shown when access is refused.
///
/// Says *when* it returns, not only that it is refused. "Wieder ab 07:00" ends
/// the conversation; "no" invites twenty minutes of arguing with a computer.
#[must_use]
pub fn deny_text(reason: Reason, retry: &RetryAt, locale: Locale) -> String {
    let head = match (reason, locale) {
        (Reason::DailyQuota, Locale::English) => "Screen time for today is used up",
        (Reason::DailyQuota, Locale::German) => "Die Bildschirmzeit für heute ist aufgebraucht",
        (Reason::WeeklyQuota, Locale::English) => "Screen time for this week is used up",
        (Reason::WeeklyQuota, Locale::German) => {
            "Die Bildschirmzeit für diese Woche ist aufgebraucht"
        }
        (Reason::OutsideWindow, Locale::English) => "Computer time is over for now",
        (Reason::OutsideWindow, Locale::German) => "Die Computerzeit ist für jetzt vorbei",
        (Reason::ServiceUnavailable, Locale::English) => {
            return "The screen time service is unavailable. Please ask a parent.".to_owned();
        }
        (Reason::ServiceUnavailable, Locale::German) => {
            return "Der Bildschirmzeit-Dienst ist nicht erreichbar. Bitte frag deine Eltern."
                .to_owned();
        }
    };

    let Some(clock) = &retry.clock else {
        return format!("{head}.");
    };

    let when = match (&retry.weekday, retry.not_today, locale) {
        (Some(day), _, _) => format!("{} {clock}", locale.weekday(day)),
        (None, true, Locale::English) => format!("tomorrow at {clock}"),
        (None, true, Locale::German) => format!("morgen um {clock}"),
        (None, false, Locale::English) => format!("at {clock}"),
        (None, false, Locale::German) => format!("um {clock}"),
    };

    match locale {
        Locale::English => format!("{head}. Available again {when}."),
        Locale::German => format!("{head}. Wieder verfügbar {when}."),
    }
}

/// The sentence shown when time is running low.
#[must_use]
pub fn warning_text(remaining_secs: u64, locale: Locale) -> String {
    let minutes = remaining_secs.div_ceil(60);
    match locale {
        Locale::English => match minutes {
            0 | 1 => "One minute left. Time to save what you're doing.".to_owned(),
            m => format!("{m} minutes left. Good moment to save what you're doing."),
        },
        Locale::German => match minutes {
            0 | 1 => "Noch eine Minute. Zeit zu speichern.".to_owned(),
            m => format!("Noch {m} Minuten. Guter Moment zum Speichern."),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(clock: &str) -> RetryAt {
        RetryAt {
            clock: Some(clock.to_owned()),
            ..RetryAt::default()
        }
    }

    #[test]
    fn locale_tags_parse_the_way_posix_writes_them() {
        assert_eq!(Locale::from_tag("de_DE.UTF-8"), Locale::German);
        assert_eq!(Locale::from_tag("de-AT"), Locale::German);
        assert_eq!(Locale::from_tag("de"), Locale::German);
        assert_eq!(Locale::from_tag("en_GB.UTF-8"), Locale::English);
        // Anything unshipped falls back rather than failing.
        assert_eq!(Locale::from_tag("fr_FR.UTF-8"), Locale::English);
        assert_eq!(Locale::from_tag("C"), Locale::English);
        assert_eq!(Locale::from_tag(""), Locale::English);
    }

    #[test]
    fn a_refusal_says_when_it_returns_in_both_languages() {
        let mut tomorrow = at("07:00");
        tomorrow.not_today = true;

        assert_eq!(
            deny_text(Reason::DailyQuota, &tomorrow, Locale::English),
            "Screen time for today is used up. Available again tomorrow at 07:00."
        );
        assert_eq!(
            deny_text(Reason::DailyQuota, &tomorrow, Locale::German),
            "Die Bildschirmzeit für heute ist aufgebraucht. Wieder verfügbar morgen um 07:00."
        );
    }

    #[test]
    fn a_window_closing_reads_as_today_when_it_reopens_today() {
        assert_eq!(
            deny_text(Reason::OutsideWindow, &at("15:00"), Locale::German),
            "Die Computerzeit ist für jetzt vorbei. Wieder verfügbar um 15:00."
        );
    }

    #[test]
    fn weekday_names_are_translated_not_passed_through() {
        // The daemon sends the English name because it does not know the
        // reader's language; the reader's side turns it into their own.
        let monday = RetryAt {
            clock: Some("04:00".to_owned()),
            not_today: true,
            weekday: Some("Monday".to_owned()),
        };
        assert!(
            deny_text(Reason::WeeklyQuota, &monday, Locale::German).contains("Montag 04:00"),
            "{}",
            deny_text(Reason::WeeklyQuota, &monday, Locale::German)
        );
        assert!(deny_text(Reason::WeeklyQuota, &monday, Locale::English).contains("Monday 04:00"));
    }

    #[test]
    fn a_refusal_without_a_time_still_reads_as_a_sentence() {
        let text = deny_text(Reason::DailyQuota, &RetryAt::default(), Locale::German);
        assert_eq!(text, "Die Bildschirmzeit für heute ist aufgebraucht.");
        assert!(!text.contains("None"), "{text}");
    }

    #[test]
    fn an_unreachable_service_says_what_to_do_about_it() {
        for locale in [Locale::English, Locale::German] {
            let text = deny_text(Reason::ServiceUnavailable, &RetryAt::default(), locale);
            assert!(!text.is_empty());
            // The child cannot fix a stopped daemon; the sentence points at
            // someone who can.
            assert!(text.contains("parent") || text.contains("Eltern"), "{text}");
        }
    }

    #[test]
    fn warnings_get_the_singular_right() {
        assert_eq!(
            warning_text(60, Locale::German),
            "Noch eine Minute. Zeit zu speichern."
        );
        assert!(warning_text(300, Locale::German).starts_with("Noch 5 Minuten"));
        assert!(warning_text(300, Locale::English).starts_with("5 minutes left"));
        // Rounded up: thirty seconds left is not "0 minutes".
        assert!(warning_text(30, Locale::English).starts_with("One minute"));
    }
}
