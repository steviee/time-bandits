// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Layouts
import QtQuick.Window
import org.kde.plasma.components as PlasmaComponents
import org.kde.kirigami as Kirigami

// Design harness: every state of the widget side by side, so they can be
// compared and screenshotted without installing anything into a live panel.
//
//   qml6 main.qml
Window {
    id: win
    visible: true
    width: layout.implicitWidth + 64
    height: layout.implicitHeight + 64
    title: "Time Bandits — widget states"
    color: Kirigami.Theme.backgroundColor

    ColumnLayout {
        id: layout
        anchors.centerIn: parent
        spacing: 28

        // ── the panel, at three remaining-time levels ─────────────
        ColumnLayout {
            Layout.alignment: Qt.AlignHCenter
            spacing: 10

            PlasmaComponents.Label {
                text: "In the panel"
                color: Kirigami.Theme.disabledTextColor
                font.pointSize: Kirigami.Theme.smallFont.pointSize
                font.letterSpacing: 1.1
            }

            Rectangle {
                Layout.alignment: Qt.AlignHCenter
                implicitWidth: panelRow.implicitWidth + 24
                implicitHeight: 44
                radius: 5
                color: Qt.tint(Kirigami.Theme.backgroundColor,
                               Qt.rgba(Kirigami.Theme.textColor.r,
                                       Kirigami.Theme.textColor.g,
                                       Kirigami.Theme.textColor.b, 0.05))
                border.width: 1
                border.color: Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g,
                                      Kirigami.Theme.textColor.b, 0.12)

                RowLayout {
                    id: panelRow
                    anchors.centerIn: parent
                    spacing: 18

                    Repeater {
                        model: [
                            { t: "1:12", c: Kirigami.Theme.highlightColor },
                            { t: "0:05", c: Kirigami.Theme.neutralTextColor },
                            { t: "0:00", c: Kirigami.Theme.negativeTextColor }
                        ]
                        delegate: Rectangle {
                            required property var modelData
                            implicitWidth: chip.implicitWidth + 18
                            implicitHeight: 26
                            radius: 4
                            color: Qt.rgba(modelData.c.r, modelData.c.g, modelData.c.b, 0.14)
                            border.width: 1
                            border.color: Qt.rgba(modelData.c.r, modelData.c.g, modelData.c.b, 0.42)

                            RowLayout {
                                id: chip
                                anchors.centerIn: parent
                                spacing: 6
                                Rectangle {
                                    width: 7; height: 7; radius: 3.5
                                    color: modelData.c
                                }
                                // The figure changes as well as the colour, so
                                // this reads for a colourblind child too.
                                PlasmaComponents.Label {
                                    text: modelData.t
                                    font.weight: Font.DemiBold
                                    font.pointSize: Kirigami.Theme.smallFont.pointSize
                                }
                            }
                        }
                    }
                }
            }
        }

        // ── the popup, in each of its states ──────────────────────
        RowLayout {
            spacing: 24
            Repeater {
                model: [
                    { m: "normal",  caption: "Time left" },
                    { m: "warning", caption: "Five minutes" },
                    { m: "blocked", caption: "Used up" },
                    { m: "privacy", caption: "What's recorded" }
                ]
                delegate: ColumnLayout {
                    required property var modelData
                    Layout.alignment: Qt.AlignTop
                    spacing: 10

                    PlasmaComponents.Label {
                        Layout.alignment: Qt.AlignHCenter
                        text: modelData.caption
                        color: Kirigami.Theme.disabledTextColor
                        font.pointSize: Kirigami.Theme.smallFont.pointSize
                        font.letterSpacing: 1.1
                    }
                    PopupMock {
                        Layout.alignment: Qt.AlignTop
                        mode: modelData.m
                    }
                }
            }
        }
    }
}
