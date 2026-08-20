// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtCore

/// Everything the widget knows, read from the agent's state file.
///
/// Plasma 6 gives QML no way to speak D-Bus, and the only alternative it does
/// offer — Plasma5Support.DataSource with the executable engine — would fork a
/// process out of the panel every few seconds. So the session agent writes a
/// small JSON file into XDG_RUNTIME_DIR and this reads it: tmpfs, owned by the
/// child, gone at logout.
///
/// A C++ QML plugin exposing the D-Bus interface directly would be the
/// KDE-idiomatic answer, and it would drag CMake into a project that
/// deliberately builds with Cargo alone. If that trade ever changes, only this
/// file does.
QtObject {
    id: root

    /// Where the agent writes. Matches tb_agent::statefile::relative_path().
    ///
    /// RuntimeLocation resolves XDG_RUNTIME_DIR, so the path is right for
    /// whichever user this instance of the widget belongs to — hard-coding a
    /// uid would break the second account on a shared machine, which is exactly
    /// the machine this product exists for.
    readonly property string path:
        StandardPaths.writableLocation(StandardPaths.RuntimeLocation)
        + "/timebandits/state.json"

    // ── what the file said ────────────────────────────────────
    property bool available: false      // the agent is running and current
    property string subject: ""
    property bool enforcement: false
    property bool blocked: false
    property int remainingSecs: -1      // -1 = unlimited
    property int usedTodaySecs: 0
    property string limitedBy: ""
    property string message: ""
    /// Wall-clock time access returns, "HH:MM", from the daemon — the only
    /// component that knows the policy's time zone.
    property string retryClock: ""
    property bool retryNotToday: false
    property bool recordTitles: false
    property var apps: []
    property string budgetKind: "daily" // daily | weekly
    property int weeklyRemainingSecs: -1
    property var week: []

    /// How long a state file may go unwritten before it stops describing now.
    /// The agent writes every tick; a minute of silence means it is gone.
    readonly property int stalenessSecs: 60

    signal loaded()

    property Timer poll: Timer {
        // Slow while collapsed — the panel shows minutes, so a faster tick
        // would only cost wakeups. The popup raises it while it is open.
        interval: 20000
        running: true
        repeat: true
        triggeredOnStart: true
        onTriggered: root.reload()
    }

    function reload() {
        const xhr = new XMLHttpRequest();
        xhr.onreadystatechange = function () {
            if (xhr.readyState !== XMLHttpRequest.DONE) {
                return;
            }
            // A local file read reports status 0 on success and on absence
            // alike; the response text is what distinguishes them.
            if (!xhr.responseText) {
                root.available = false;
                root.loaded();
                return;
            }
            try {
                root.apply(JSON.parse(xhr.responseText));
            } catch (e) {
                // A half-written file is normal: the agent replaces it
                // atomically, but a reader can still catch a truncated read on
                // some filesystems. Keeping the previous state beats flickering.
                console.warn("time-bandits: unreadable state file:", e);
            }
            root.loaded();
        };
        try {
            xhr.open("GET", root.path);
            xhr.send();
        } catch (e) {
            root.available = false;
            root.loaded();
        }
    }

    function apply(s) {
        const age = Math.floor(Date.now() / 1000) - (s.updated || 0);
        if (age > root.stalenessSecs) {
            root.available = false;
            return;
        }
        root.available = true;
        root.subject = s.subject || "";
        root.enforcement = !!s.enforcement;
        root.blocked = !!s.blocked;
        root.remainingSecs = (s.remaining_secs === null || s.remaining_secs === undefined)
                             ? -1 : s.remaining_secs;
        root.usedTodaySecs = s.used_today_secs || 0;
        root.limitedBy = s.limited_by || "";
        root.message = s.message || "";
        const retry = s.retry || {};
        root.retryClock = retry.clock || "";
        root.retryNotToday = !!retry.not_today;
        root.recordTitles = !!s.record_titles;
        root.apps = s.apps || [];
        const w = s.week || {};
        root.budgetKind = w.budget || "daily";
        root.weeklyRemainingSecs = (w.weekly_remaining_secs === null
                                    || w.weekly_remaining_secs === undefined)
                                   ? -1 : w.weekly_remaining_secs;
        root.week = w.days || [];
    }

    // ── formatting, shared by both representations ────────────

    /// "1:12" for the panel. Hours and minutes, never seconds — a counter that
    /// ticks every second is a counter a child watches instead of working.
    function shortTime(secs) {
        if (secs < 0) {
            return "∞";
        }
        const m = Math.ceil(secs / 60);
        return Math.floor(m / 60) + ":" + ("0" + (m % 60)).slice(-2);
    }

    /// "1 h 12 min" for the popup.
    function longTime(secs) {
        if (secs < 0) {
            return i18n("unlimited");
        }
        const m = Math.ceil(secs / 60);
        const h = Math.floor(m / 60);
        if (h === 0) {
            return i18np("%1 min", "%1 min", m % 60);
        }
        if (m % 60 === 0) {
            return i18np("%1 h", "%1 h", h);
        }
        return i18nc("hours and minutes, e.g. 1 h 12 min", "%1 h %2 min", h, m % 60);
    }
}
