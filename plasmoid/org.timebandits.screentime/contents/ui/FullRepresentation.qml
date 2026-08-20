// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Layouts
import org.kde.plasma.components as PlasmaComponents
import org.kde.plasma.extras as PlasmaExtras
import org.kde.kirigami as Kirigami
import org.timebandits.screentime.private

/// The popup: how long is left, where it went, and what the week holds.
ColumnLayout {
    id: root

    required property var fmt
    property bool showingPrivacy: false

    Layout.minimumWidth: Kirigami.Units.gridUnit * 20
    Layout.minimumHeight: Kirigami.Units.gridUnit * 26
    spacing: 0

    readonly property bool nearlyUp: ScreenTimeAgent.remainingSeconds >= 0
                                     && ScreenTimeAgent.remainingSeconds <= 300
    readonly property color accent: ScreenTimeAgent.blocked
        ? Kirigami.Theme.negativeTextColor
        : nearlyUp ? Kirigami.Theme.neutralTextColor
                   : Kirigami.Theme.highlightColor

    PlasmaExtras.PlaceholderMessage {
        Layout.fillWidth: true
        Layout.fillHeight: true
        Layout.margins: Kirigami.Units.largeSpacing
        visible: !ScreenTimeAgent.available
        iconName: "dialog-warning"
        text: i18n("Not connected")
        explanation: i18n("The screen time service is not running on this computer. Ask a parent to check it.")
    }

    // ── what is recorded ──────────────────────────────────────
    ColumnLayout {
        Layout.fillWidth: true
        Layout.fillHeight: true
        Layout.margins: Kirigami.Units.largeSpacing
        visible: ScreenTimeAgent.available && root.showingPrivacy
        spacing: Kirigami.Units.smallSpacing

        PlasmaComponents.Label {
            text: i18n("What's recorded about you")
            font.weight: Font.DemiBold
        }
        PlasmaComponents.Label {
            text: i18n("Your parents can see this.")
            color: Kirigami.Theme.disabledTextColor
            font.pointSize: Kirigami.Theme.smallFont.pointSize
            Layout.bottomMargin: Kirigami.Units.smallSpacing
        }

        Repeater {
            model: [
                { on: true, t: i18n("Which app is in front, and for how long"),
                  s: i18n("The names, not the contents") },
                { on: true, t: i18n("When you stop using the computer"),
                  s: i18n("So time away doesn't count against you") },
                { on: ScreenTimeAgent.recordTitles, t: i18n("Window titles"),
                  s: ScreenTimeAgent.recordTitles
                     ? i18n("Currently on. Your parents switched this on.")
                     : i18n("Currently off. If your parents switch it on, this line changes.") },
                { on: false, t: i18n("What you type, browse, or say"),
                  s: i18n("Never recorded. There is no code that could.") },
                { on: false, t: i18n("Screenshots, camera, microphone"),
                  s: i18n("Never recorded.") }
            ]
            delegate: RowLayout {
                required property var modelData
                Layout.fillWidth: true
                spacing: Kirigami.Units.largeSpacing

                Kirigami.Icon {
                    source: modelData.on ? "checkmark" : "dialog-cancel"
                    Layout.preferredWidth: Kirigami.Units.iconSizes.small
                    Layout.preferredHeight: Kirigami.Units.iconSizes.small
                    Layout.alignment: Qt.AlignTop
                    color: modelData.on ? Kirigami.Theme.positiveTextColor
                                        : Kirigami.Theme.disabledTextColor
                    isMask: true
                }
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 1
                    PlasmaComponents.Label {
                        text: modelData.t
                        font.pointSize: Kirigami.Theme.smallFont.pointSize
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                    }
                    PlasmaComponents.Label {
                        text: modelData.s
                        color: Kirigami.Theme.disabledTextColor
                        font.pointSize: Kirigami.Theme.smallFont.pointSize * 0.92
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                    }
                }
            }
        }
        Item { Layout.fillHeight: true }
    }

    // ── the main page ─────────────────────────────────────────
    ColumnLayout {
        Layout.fillWidth: true
        Layout.fillHeight: true
        visible: ScreenTimeAgent.available && !root.showingPrivacy
        spacing: 0

        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: banner.implicitHeight + Kirigami.Units.largeSpacing * 1.6
            visible: ScreenTimeAgent.blocked || root.nearlyUp
            color: Qt.rgba(root.accent.r, root.accent.g, root.accent.b, 0.13)

            RowLayout {
                id: banner
                anchors.fill: parent
                anchors.margins: Kirigami.Units.largeSpacing
                spacing: Kirigami.Units.largeSpacing

                Kirigami.Icon {
                    source: ScreenTimeAgent.blocked ? "lock" : "clock"
                    Layout.preferredWidth: Kirigami.Units.iconSizes.smallMedium
                    Layout.preferredHeight: Kirigami.Units.iconSizes.smallMedium
                    color: root.accent
                    isMask: true
                }
                PlasmaComponents.Label {
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    font.pointSize: Kirigami.Theme.smallFont.pointSize
                    text: ScreenTimeAgent.blocked
                          ? ScreenTimeAgent.message
                          : i18n("Time is nearly up. Good moment to save what you're doing.")
                }
            }
        }

        /// Observing only. Pretending otherwise would be a lie the child could
        /// catch the first time nothing happened at zero.
        PlasmaComponents.Label {
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.largeSpacing
            visible: !ScreenTimeAgent.enforcement
            wrapMode: Text.WordWrap
            font.pointSize: Kirigami.Theme.smallFont.pointSize
            color: Kirigami.Theme.disabledTextColor
            text: i18n("Your time is being recorded, but nothing is limited at the moment.")
        }

        TimeRing {
            Layout.alignment: Qt.AlignHCenter
            Layout.topMargin: Kirigami.Units.largeSpacing * 1.5
            accent: root.accent
            /// A closed ring when blocked, not an empty one: an empty track
            /// reads as a broken widget rather than a spent budget.
            fraction: {
                if (ScreenTimeAgent.blocked || ScreenTimeAgent.remainingSeconds < 0) {
                    return 1;
                }
                const total = ScreenTimeAgent.remainingSeconds + ScreenTimeAgent.usedTodaySeconds;
                return total > 0 ? ScreenTimeAgent.remainingSeconds / total : 0;
            }
            /// Blocked shows when time comes back, not a zero. Zero is a dead
            /// end; a time is something a child can plan around.
            bigText: ScreenTimeAgent.blocked
                     ? (ScreenTimeAgent.retryClock || i18n("later"))
                     : root.fmt.long(ScreenTimeAgent.remainingSeconds)
            unitText: ScreenTimeAgent.blocked ? i18n("BACK AT") : i18n("LEFT")
        }

        PlasmaComponents.Label {
            Layout.alignment: Qt.AlignHCenter
            Layout.topMargin: Kirigami.Units.smallSpacing
            color: Kirigami.Theme.disabledTextColor
            font.pointSize: Kirigami.Theme.smallFont.pointSize
            text: i18n("%1 used today", root.fmt.long(ScreenTimeAgent.usedTodaySeconds))
        }

        WeekStrip {
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.largeSpacing
            fmt: root.fmt
            visible: ScreenTimeAgent.week.length > 0
        }

        /// Where the time went. The child sees the same breakdown a parent does.
        ColumnLayout {
            Layout.fillWidth: true
            Layout.leftMargin: Kirigami.Units.largeSpacing
            Layout.rightMargin: Kirigami.Units.largeSpacing
            visible: ScreenTimeAgent.apps.length > 0
            spacing: Kirigami.Units.smallSpacing

            PlasmaComponents.Label {
                text: i18n("TODAY")
                color: Kirigami.Theme.disabledTextColor
                font.pointSize: Kirigami.Theme.smallFont.pointSize * 0.85
                font.letterSpacing: 1.1
                font.weight: Font.DemiBold
            }
            Repeater {
                model: ScreenTimeAgent.apps.slice(0, 4)
                delegate: AppRow {
                    required property var modelData
                    Layout.fillWidth: true
                    appName: modelData.id === "unknown"
                             ? i18n("Something else") : modelData.name
                    duration: root.fmt.long(modelData.seconds)
                    share: ScreenTimeAgent.apps[0].seconds > 0
                           ? modelData.seconds / ScreenTimeAgent.apps[0].seconds : 0
                    unattributed: modelData.id === "unknown"
                    swatch: modelData.id === "unknown"
                            ? Kirigami.Theme.disabledTextColor
                            : Kirigami.Theme.highlightColor
                }
            }

            /// Incomplete data is said out loud rather than shown as if whole.
            PlasmaComponents.Label {
                Layout.fillWidth: true
                Layout.topMargin: Kirigami.Units.smallSpacing
                visible: !ScreenTimeAgent.focusTracking
                wrapMode: Text.WordWrap
                color: Kirigami.Theme.disabledTextColor
                font.pointSize: Kirigami.Theme.smallFont.pointSize * 0.9
                text: i18n("Some time can't be matched to an app right now.")
            }
        }

        Item { Layout.fillHeight: true }
    }

    RowLayout {
        Layout.fillWidth: true
        Layout.margins: Kirigami.Units.smallSpacing
        visible: ScreenTimeAgent.available
        spacing: Kirigami.Units.smallSpacing

        PlasmaComponents.Button {
            Layout.fillWidth: true
            visible: !root.showingPrivacy && ScreenTimeAgent.enforcement
            text: i18n("Ask for more time")
            icon.name: "appointment-new"
            /// Wired to the agent next; shown disabled rather than absent so
            /// the popup does not change shape when it starts working.
            enabled: false
        }
        PlasmaComponents.Button {
            Layout.fillWidth: root.showingPrivacy
            flat: true
            text: root.showingPrivacy ? i18n("Back") : i18n("What's recorded")
            onClicked: root.showingPrivacy = !root.showingPrivacy
        }
    }
}
