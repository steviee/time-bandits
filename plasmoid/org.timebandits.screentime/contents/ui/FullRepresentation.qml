// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Layouts
import org.kde.plasma.components as PlasmaComponents
import org.kde.plasma.extras as PlasmaExtras
import org.kde.kirigami as Kirigami

/// The popup: how long is left, where it went, and what the week holds.
ColumnLayout {
    id: root

    required property var state
    property bool showingPrivacy: false

    Layout.minimumWidth: Kirigami.Units.gridUnit * 20
    Layout.minimumHeight: Kirigami.Units.gridUnit * 24
    spacing: 0

    readonly property color accent: state.blocked ? Kirigami.Theme.negativeTextColor
        : (state.remainingSecs >= 0 && state.remainingSecs <= 300)
            ? Kirigami.Theme.neutralTextColor
            : Kirigami.Theme.highlightColor

    // ── nothing to show ───────────────────────────────────────
    PlasmaExtras.PlaceholderMessage {
        Layout.fillWidth: true
        Layout.fillHeight: true
        visible: !root.state.available
        iconName: "dialog-warning"
        text: i18n("Not connected")
        explanation: i18n("The screen time service is not running on this computer. Ask a parent to check it.")
    }

    // ── the transparency page ─────────────────────────────────
    ColumnLayout {
        Layout.fillWidth: true
        Layout.fillHeight: true
        Layout.margins: Kirigami.Units.largeSpacing
        visible: root.state.available && root.showingPrivacy
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
                { on: root.state.recordTitles, t: i18n("Window titles"),
                  s: root.state.recordTitles
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
        visible: root.state.available && !root.showingPrivacy
        spacing: 0

        // Blocked or nearly so: say it before anything else.
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: banner.implicitHeight + Kirigami.Units.largeSpacing * 1.6
            visible: root.state.blocked
                     || (root.state.remainingSecs >= 0 && root.state.remainingSecs <= 300)
            color: Qt.rgba(root.accent.r, root.accent.g, root.accent.b, 0.13)

            RowLayout {
                id: banner
                anchors.fill: parent
                anchors.margins: Kirigami.Units.largeSpacing
                spacing: Kirigami.Units.largeSpacing

                Kirigami.Icon {
                    source: root.state.blocked ? "lock" : "clock"
                    Layout.preferredWidth: Kirigami.Units.iconSizes.smallMedium
                    Layout.preferredHeight: Kirigami.Units.iconSizes.smallMedium
                    color: root.accent
                    isMask: true
                }
                PlasmaComponents.Label {
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    font.pointSize: Kirigami.Theme.smallFont.pointSize
                    text: root.state.blocked
                          ? root.state.message
                          : i18n("Time is nearly up. Good moment to save what you're doing.")
                }
            }
        }

        // Observing only: a parent has switched enforcement off, and pretending
        // otherwise would be a lie the child could catch.
        PlasmaComponents.Label {
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.largeSpacing
            visible: !root.state.enforcement
            wrapMode: Text.WordWrap
            font.pointSize: Kirigami.Theme.smallFont.pointSize
            color: Kirigami.Theme.disabledTextColor
            text: i18n("Your time is being recorded, but nothing is limited at the moment.")
        }

        TimeRing {
            Layout.alignment: Qt.AlignHCenter
            Layout.topMargin: Kirigami.Units.largeSpacing * 1.5
            accent: root.accent
            // Blocked draws a closed ring rather than an empty one: an empty
            // track reads as a broken widget, not as a spent budget.
            fraction: {
                if (root.state.blocked) {
                    return 1;
                }
                if (root.state.remainingSecs < 0) {
                    return 1;
                }
                const total = root.state.remainingSecs + root.state.usedTodaySecs;
                return total > 0 ? root.state.remainingSecs / total : 0;
            }
            bigText: root.state.blocked
                     ? (root.state.retryClock || i18n("later"))
                     : root.state.longTime(root.state.remainingSecs)
            unitText: root.state.blocked ? i18n("BACK AT") : i18n("LEFT")
        }

        PlasmaComponents.Label {
            Layout.alignment: Qt.AlignHCenter
            Layout.topMargin: Kirigami.Units.smallSpacing
            color: Kirigami.Theme.disabledTextColor
            font.pointSize: Kirigami.Theme.smallFont.pointSize
            text: i18n("%1 used today", root.state.longTime(root.state.usedTodaySecs))
        }

        WeekStrip {
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.largeSpacing
            state: root.state
            visible: root.state.week.length > 0
        }

        // Where the time went. The child sees the same breakdown a parent does.
        ColumnLayout {
            Layout.fillWidth: true
            Layout.leftMargin: Kirigami.Units.largeSpacing
            Layout.rightMargin: Kirigami.Units.largeSpacing
            visible: root.state.apps.length > 0
            spacing: Kirigami.Units.smallSpacing

            PlasmaComponents.Label {
                text: i18n("TODAY")
                color: Kirigami.Theme.disabledTextColor
                font.pointSize: Kirigami.Theme.smallFont.pointSize * 0.85
                font.letterSpacing: 1.1
                font.weight: Font.DemiBold
            }
            Repeater {
                model: root.state.apps.slice(0, 4)
                delegate: AppRow {
                    required property var modelData
                    Layout.fillWidth: true
                    appName: modelData.name || modelData.id
                    duration: root.state.longTime(modelData.secs)
                    share: root.state.apps[0].secs > 0
                           ? modelData.secs / root.state.apps[0].secs : 0
                    unattributed: modelData.id === "unknown"
                    swatch: modelData.id === "unknown"
                            ? Kirigami.Theme.disabledTextColor
                            : Kirigami.Theme.highlightColor
                }
            }
        }

        Item { Layout.fillHeight: true }
    }

    // ── footer ────────────────────────────────────────────────
    RowLayout {
        Layout.fillWidth: true
        Layout.margins: Kirigami.Units.smallSpacing
        visible: root.state.available
        spacing: Kirigami.Units.smallSpacing

        PlasmaComponents.Button {
            Layout.fillWidth: true
            visible: !root.showingPrivacy && root.state.enforcement
            text: i18n("Ask for more time")
            icon.name: "appointment-new"
            // Wired to the agent in the next step; disabled rather than absent
            // so the popup does not change shape when it starts working.
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
