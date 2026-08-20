// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Layouts
import org.kde.plasma.components as PlasmaComponents
import org.kde.plasma.extras as PlasmaExtras
import org.kde.kirigami as Kirigami

// The plasmoid's full representation, in one of its four states.
//
// Rendered here inside a plain Window rather than a PlasmoidItem, so it can be
// run and screenshotted without installing anything into a live panel. Every
// component, colour and metric is the real one — this is what Plasma will draw.
Rectangle {
    id: root

    // normal | warning | blocked | privacy
    property string mode: "normal"
    property string childName: "Alice"

    readonly property bool blocked: mode === "blocked"
    readonly property bool warning: mode === "warning"

    readonly property color accent: blocked ? Kirigami.Theme.negativeTextColor
                                 : warning ? Kirigami.Theme.neutralTextColor
                                           : Kirigami.Theme.highlightColor

    implicitWidth: Kirigami.Units.gridUnit * 20
    implicitHeight: content.implicitHeight
    color: Kirigami.Theme.backgroundColor
    radius: 6
    border.width: 1
    border.color: Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g,
                          Kirigami.Theme.textColor.b, 0.14)

    ColumnLayout {
        id: content
        anchors.fill: parent
        anchors.margins: 0
        spacing: 0

        // ── header ────────────────────────────────────────────────
        RowLayout {
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.largeSpacing
            spacing: Kirigami.Units.largeSpacing

            Rectangle {
                Layout.preferredWidth: Kirigami.Units.gridUnit * 1.8
                Layout.preferredHeight: Kirigami.Units.gridUnit * 1.8
                radius: width / 2
                gradient: Gradient {
                    GradientStop { position: 0; color: "#6f42c1" }
                    GradientStop { position: 1; color: "#3daee9" }
                }
                PlasmaComponents.Label {
                    anchors.centerIn: parent
                    text: root.childName.charAt(0)
                    color: "white"
                    font.bold: true
                }
            }

            ColumnLayout {
                Layout.fillWidth: true
                spacing: 0
                PlasmaComponents.Label {
                    text: root.mode === "privacy"
                          ? i18nMock("What's recorded about you")
                          : root.childName
                    font.weight: Font.DemiBold
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                }
                PlasmaComponents.Label {
                    text: root.mode === "privacy"
                          ? i18nMock("Visible to: Mum, Dad")
                          : (root.blocked ? i18nMock("Wednesday")
                                          : i18nMock("Wednesday · until 19:00"))
                    color: Kirigami.Theme.disabledTextColor
                    font.pointSize: Kirigami.Theme.smallFont.pointSize
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                }
            }
        }

        Rectangle {
            Layout.fillWidth: true
            height: 1
            color: Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g,
                           Kirigami.Theme.textColor.b, 0.11)
        }

        // ── banner, when something is happening ───────────────────
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: bannerRow.implicitHeight + Kirigami.Units.largeSpacing * 1.6
            visible: root.warning || root.blocked
            color: Qt.rgba(root.accent.r, root.accent.g, root.accent.b, 0.13)

            RowLayout {
                id: bannerRow
                anchors.fill: parent
                anchors.margins: Kirigami.Units.largeSpacing
                spacing: Kirigami.Units.largeSpacing

                Kirigami.Icon {
                    source: root.blocked ? "lock" : "clock"
                    Layout.preferredWidth: Kirigami.Units.iconSizes.smallMedium
                    Layout.preferredHeight: Kirigami.Units.iconSizes.smallMedium
                    color: root.accent
                    isMask: true
                }
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 1
                    PlasmaComponents.Label {
                        text: root.blocked ? i18nMock("Screen time is used up")
                                           : i18nMock("Five minutes left")
                        font.weight: Font.DemiBold
                        font.pointSize: Kirigami.Theme.smallFont.pointSize
                    }
                    PlasmaComponents.Label {
                        text: root.blocked
                              ? i18nMock("The screen will lock in about a minute.")
                              : i18nMock("Good moment to save what you're doing.")
                        color: Kirigami.Theme.disabledTextColor
                        font.pointSize: Kirigami.Theme.smallFont.pointSize
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                    }
                }
            }
        }

        // ── the ring ──────────────────────────────────────────────
        ColumnLayout {
            Layout.fillWidth: true
            Layout.topMargin: Kirigami.Units.largeSpacing * 1.5
            Layout.bottomMargin: Kirigami.Units.largeSpacing
            visible: root.mode !== "privacy"
            spacing: Kirigami.Units.largeSpacing

            TimeRing {
                Layout.alignment: Qt.AlignHCenter
                accent: root.accent
                // A closed ring rather than an empty one. An empty track reads
                // as "disabled" — as though the widget had stopped working —
                // when the point is that the budget is spent and shut.
                fraction: root.blocked ? 1 : (root.warning ? 0.04 : 0.6)
                // Blocked shows when access comes back, not a zero. Zero is a
                // dead end; a time is something a child can plan around.
                bigText: root.blocked ? "07:00" : (root.warning ? "5" : "1 h 12")
                unitText: root.blocked ? i18nMock("BACK TOMORROW")
                                       : i18nMock("MINUTES LEFT")
            }

            PlasmaComponents.Label {
                Layout.alignment: Qt.AlignHCenter
                horizontalAlignment: Text.AlignHCenter
                color: Kirigami.Theme.disabledTextColor
                font.pointSize: Kirigami.Theme.smallFont.pointSize
                text: root.blocked ? i18nMock("You used 2 h today")
                     : root.warning ? i18nMock("Screen locks at 17:00")
                                    : i18nMock("of 2 h today · used 48 min")
            }
        }

        // ── today's applications ──────────────────────────────────
        ColumnLayout {
            Layout.fillWidth: true
            Layout.leftMargin: Kirigami.Units.largeSpacing
            Layout.rightMargin: Kirigami.Units.largeSpacing
            Layout.bottomMargin: Kirigami.Units.largeSpacing
            visible: root.mode === "normal"
            spacing: Kirigami.Units.smallSpacing * 1.5

            PlasmaComponents.Label {
                text: i18nMock("TODAY")
                color: Kirigami.Theme.disabledTextColor
                font.pointSize: Kirigami.Theme.smallFont.pointSize * 0.85
                font.letterSpacing: 1.1
                font.weight: Font.DemiBold
            }
            AppRow {
                Layout.fillWidth: true
                appName: "Firefox"; duration: "27 min"; share: 1.0; swatch: "#e66000"
            }
            AppRow {
                Layout.fillWidth: true
                appName: "Minecraft"; duration: "15 min"; share: 0.55; swatch: "#5a9e3f"
            }
            AppRow {
                Layout.fillWidth: true
                // Time the daemon could not attribute gets its own line rather
                // than being folded into the last known app or quietly dropped.
                appName: i18nMock("Something else"); duration: "6 min"
                share: 0.22; unattributed: true
                swatch: Kirigami.Theme.disabledTextColor
            }
        }

        // ── what is recorded ──────────────────────────────────────
        ColumnLayout {
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.largeSpacing
            visible: root.mode === "privacy"
            spacing: 0

            Repeater {
                model: [
                    { on: true,  t: i18nMock("Which app is in front, and for how long"),
                      s: i18nMock("The names, not the contents") },
                    { on: true,  t: i18nMock("When you stop using the computer"),
                      s: i18nMock("So time away doesn't count against you") },
                    { on: false, t: i18nMock("Window titles — off"),
                      s: i18nMock("Your parents can turn this on. You'd see it change here.") },
                    { on: false, t: i18nMock("What you type, browse, or say"),
                      s: i18nMock("Never recorded. There is no code that could.") },
                    { on: false, t: i18nMock("Screenshots, camera, microphone"),
                      s: i18nMock("Never recorded.") }
                ]
                delegate: RowLayout {
                    required property var modelData
                    Layout.fillWidth: true
                    Layout.topMargin: Kirigami.Units.smallSpacing
                    Layout.bottomMargin: Kirigami.Units.smallSpacing
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
        }

        Rectangle {
            Layout.fillWidth: true
            height: 1
            color: Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g,
                           Kirigami.Theme.textColor.b, 0.11)
        }

        // ── footer ────────────────────────────────────────────────
        RowLayout {
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing * 1.5
            spacing: Kirigami.Units.smallSpacing

            PlasmaComponents.Button {
                Layout.fillWidth: true
                visible: root.mode !== "privacy"
                // Specific, not open-ended: "ask for 15 more minutes" is one
                // decision, "ask for more time" is a negotiation.
                text: root.blocked ? i18nMock("Ask a parent")
                     : root.warning ? i18nMock("Ask for 15 more minutes")
                                    : i18nMock("Ask for more time")
                highlighted: root.warning || root.blocked
            }
            PlasmaComponents.Button {
                Layout.fillWidth: root.mode === "privacy"
                flat: true
                text: root.mode === "privacy" ? i18nMock("Back") : i18nMock("What's recorded")
            }
        }
    }

    // Stand-in for the real i18n() call, which only exists inside a plasmoid.
    // Keeping every string wrapped means the switch to real translation is a
    // find-and-replace rather than a rewrite.
    function i18nMock(s) { return s; }
}
