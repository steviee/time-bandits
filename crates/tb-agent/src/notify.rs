// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Telling the child what is happening, on the desktop.
//!
//! This is the one channel that does not depend on the widget being in the
//! panel — which matters, because a child may remove it and the warning is the
//! part they must not lose. A limit that arrives without warning is an ambush,
//! and a child ambushed once spends the rest of the year trying to defeat the
//! thing that ambushed them.
//!
//! Verified against the running service rather than the specification: Plasma's
//! notification server reports `actions` and `inline-reply` among its
//! capabilities, so the warning can carry a button and the child can add a
//! reason without leaving what they are doing.

use std::collections::HashMap;

use tb_proto::text::{Locale, warning_text};
use zbus::zvariant::Value;

/// The action key sent back when the child presses the button.
pub const ACTION_MORE_TIME: &str = "tb-more-time";

/// How much time the button asks for. One decision rather than a negotiation:
/// "ask for fifteen more minutes" is a thing a child can press, "ask for more
/// time" is a conversation.
pub const REQUEST_MINUTES: u64 = 15;

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
pub trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: &str) -> zbus::Result<()>;
}

/// What a notification is about, which decides how insistent it should be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Time is running low.
    Warning,
    /// Access has just been refused.
    Blocked,
    /// Access has just come back.
    Restored,
}

impl Kind {
    /// Freedesktop urgency: 0 low, 1 normal, 2 critical.
    const fn urgency(self) -> u8 {
        match self {
            // Critical notifications survive do-not-disturb, which is right for
            // the two that change what the child can do, and wrong for the one
            // that is merely good news.
            Self::Warning | Self::Blocked => 2,
            Self::Restored => 1,
        }
    }

    const fn icon(self) -> &'static str {
        match self {
            Self::Warning => "clock",
            Self::Blocked => "lock",
            Self::Restored => "dialog-positive",
        }
    }

    /// Whether the child can do anything about it from here.
    const fn offers_request(self) -> bool {
        matches!(self, Self::Warning | Self::Blocked)
    }
}

/// Sends notifications, replacing its own rather than stacking them.
#[derive(Debug)]
pub struct Notifier {
    proxy: NotificationsProxy<'static>,
    /// The id of the last notification, so an update replaces it. Without this
    /// a child gets a fresh banner at fifteen, five and one minute, plus one
    /// when it locks — four things to dismiss instead of one that changes.
    last_id: u32,
}

impl Notifier {
    pub async fn new(connection: &zbus::Connection) -> zbus::Result<Self> {
        Ok(Self {
            proxy: NotificationsProxy::new(connection).await?,
            last_id: 0,
        })
    }

    /// Puts one message on screen.
    ///
    /// Failures are logged and swallowed: a notification server that is not
    /// running is a reason to lose the message, never a reason to stop
    /// reporting or enforcing.
    pub async fn show(&mut self, kind: Kind, summary: &str, body: &str, locale: Locale) {
        let ask = match locale {
            Locale::German => "Frag nach 15 Minuten mehr",
            Locale::English => "Ask for 15 more minutes",
        };
        let actions: Vec<&str> = if kind.offers_request() {
            vec![ACTION_MORE_TIME, ask]
        } else {
            Vec::new()
        };

        let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
        hints.insert("urgency", Value::U8(kind.urgency()));
        // Groups our notifications together in the history rather than
        // scattering them among everything else the desktop said today.
        hints.insert("x-kde-origin-name", Value::from("Time Bandits"));

        let result = self
            .proxy
            .notify(
                "Time Bandits",
                self.last_id,
                kind.icon(),
                summary,
                body,
                &actions,
                hints,
                // Critical notifications stay until dismissed; the good news
                // can go away on its own.
                if kind.urgency() == 2 { 0 } else { 8000 },
            )
            .await;

        match result {
            Ok(id) => self.last_id = id,
            Err(e) => tracing::warn!(error = %e, "could not show a notification"),
        }
    }

    /// The warning that time is running low.
    pub async fn warn(&mut self, remaining_secs: u64, locale: Locale) {
        let summary = match locale {
            Locale::German => "Bildschirmzeit",
            Locale::English => "Screen time",
        };
        self.show(
            Kind::Warning,
            summary,
            &warning_text(remaining_secs, locale),
            locale,
        )
        .await;
    }

    /// The message when time has run out. `body` comes from the daemon's facts,
    /// already written in this session's language.
    pub async fn blocked(&mut self, body: &str, locale: Locale) {
        let summary = match locale {
            Locale::German => "Bildschirmzeit aufgebraucht",
            Locale::English => "Screen time is up",
        };
        self.show(Kind::Blocked, summary, body, locale).await;
    }

    /// The message when a parent has granted more time.
    pub async fn restored(&mut self, locale: Locale) {
        let (summary, body) = match locale {
            Locale::German => (
                "Du hast wieder Zeit",
                "Ein Elternteil hat dir mehr Zeit gegeben.",
            ),
            Locale::English => ("Your time is back", "A parent has given you more time."),
        };
        self.show(Kind::Restored, summary, body, locale).await;
    }

    /// Listens for the child pressing the button.
    pub async fn action_stream(&self) -> zbus::Result<ActionInvokedStream> {
        self.proxy.receive_action_invoked().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_actionable_kinds_offer_a_button() {
        // Offering "ask for more time" on the message that says a parent just
        // gave you more time would be daft.
        assert!(Kind::Warning.offers_request());
        assert!(Kind::Blocked.offers_request());
        assert!(!Kind::Restored.offers_request());
    }

    #[test]
    fn the_messages_that_change_what_a_child_can_do_are_critical() {
        // Critical survives do-not-disturb. Being locked out without warning
        // because the desktop was quiet is the failure this prevents.
        assert_eq!(Kind::Warning.urgency(), 2);
        assert_eq!(Kind::Blocked.urgency(), 2);
        assert_eq!(Kind::Restored.urgency(), 1, "good news can wait");
    }

    #[test]
    fn every_kind_names_an_icon_from_the_theme() {
        for kind in [Kind::Warning, Kind::Blocked, Kind::Restored] {
            let icon = kind.icon();
            assert!(!icon.is_empty());
            assert!(!icon.contains('/'), "a theme name, not a path: {icon}");
        }
    }
}
