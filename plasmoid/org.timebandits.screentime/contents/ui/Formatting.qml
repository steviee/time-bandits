// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick

/// Turning seconds into something a child reads.
///
/// A QML object rather than a JavaScript library because every string here goes
/// through i18n(), which a `pragma library` cannot reach.
QtObject {
    /// "1:12" for the panel. Hours and minutes, never seconds — a counter that
    /// ticks every second is one a child watches instead of working.
    function short(secs) {
        if (secs < 0) {
            return "∞";
        }
        const m = Math.ceil(secs / 60);
        return Math.floor(m / 60) + ":" + ("0" + (m % 60)).slice(-2);
    }

    /// "1 h 12 min" for the popup.
    function long(secs) {
        if (secs < 0) {
            return i18n("unlimited");
        }
        const m = Math.ceil(secs / 60);
        const h = Math.floor(m / 60);
        if (h === 0) {
            return i18np("%1 minute", "%1 minutes", m);
        }
        if (m % 60 === 0) {
            return i18np("%1 hour", "%1 hours", h);
        }
        return i18nc("hours and minutes, e.g. 1 h 12 min", "%1 h %2 min", h, m % 60);
    }

    /// Two letters for the week strip. The agent sends English weekday names
    /// because it cannot know the reader's language; this is where they become
    /// the reader's own.
    function weekdayShort(english) {
        switch (english) {
        case "monday":    return i18nc("Monday, two letters", "Mo");
        case "tuesday":   return i18nc("Tuesday, two letters", "Tu");
        case "wednesday": return i18nc("Wednesday, two letters", "We");
        case "thursday":  return i18nc("Thursday, two letters", "Th");
        case "friday":    return i18nc("Friday, two letters", "Fr");
        case "saturday":  return i18nc("Saturday, two letters", "Sa");
        case "sunday":    return i18nc("Sunday, two letters", "Su");
        default:          return "";
        }
    }
}
