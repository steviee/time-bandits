// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Layouts
import org.kde.plasma.components as PlasmaComponents
import org.kde.kirigami as Kirigami

// One application's share of the day. The child sees exactly the breakdown a
// parent sees — same data, same names.
RowLayout {
    id: root

    property string appName: ""
    property string duration: ""
    property real share: 0            // 0..1 of the largest entry
    property color swatch: Kirigami.Theme.highlightColor
    property bool unattributed: false

    spacing: Kirigami.Units.smallSpacing * 2

    Rectangle {
        Layout.preferredWidth: Kirigami.Units.iconSizes.small
        Layout.preferredHeight: Kirigami.Units.iconSizes.small
        radius: 3
        color: root.swatch
        opacity: root.unattributed ? 0.55 : 1
    }

    ColumnLayout {
        Layout.fillWidth: true
        spacing: 3

        PlasmaComponents.Label {
            Layout.fillWidth: true
            text: root.appName
            font.pointSize: Kirigami.Theme.smallFont.pointSize
            font.italic: root.unattributed
            elide: Text.ElideRight
        }
        Rectangle {
            Layout.fillWidth: true
            height: 4
            radius: 2
            color: Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g,
                           Kirigami.Theme.textColor.b, 0.09)
            Rectangle {
                width: parent.width * root.share
                height: parent.height
                radius: parent.radius
                color: root.unattributed ? Kirigami.Theme.disabledTextColor : root.swatch
            }
        }
    }

    PlasmaComponents.Label {
        text: root.duration
        color: Kirigami.Theme.disabledTextColor
        font.pointSize: Kirigami.Theme.smallFont.pointSize
        font.features: { "tnum": 1 }
    }
}
