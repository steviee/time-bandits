// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Layouts
import org.kde.plasma.components as PlasmaComponents
import org.kde.kirigami as Kirigami

/// What sits in the panel: remaining time, counting down.
///
/// Elapsed time would be useless — a child cannot do arithmetic against a quota
/// they cannot see. The colour changes with the figure rather than instead of
/// it, so this reads for a colourblind child and in a screenshot.
MouseArea {
    id: root

    required property var state

    readonly property color accent: state.blocked ? Kirigami.Theme.negativeTextColor
        : (state.remainingSecs >= 0 && state.remainingSecs <= 300)
            ? Kirigami.Theme.neutralTextColor
            : Kirigami.Theme.highlightColor

    // Nothing to say for an unmanaged account. A reassuring zero would be worse
    // than an absent widget.
    readonly property bool hasSomethingToSay: state.available && state.enforcement

    implicitWidth: hasSomethingToSay ? chip.implicitWidth + Kirigami.Units.largeSpacing
                                     : Kirigami.Units.iconSizes.medium
    implicitHeight: Kirigami.Units.iconSizes.medium
    hoverEnabled: true

    Kirigami.Icon {
        anchors.centerIn: parent
        visible: !root.hasSomethingToSay
        source: "preferences-system-time"
        width: Kirigami.Units.iconSizes.small
        height: width
        opacity: root.state.available ? 0.6 : 0.35
    }

    Rectangle {
        anchors.centerIn: parent
        visible: root.hasSomethingToSay
        implicitWidth: chip.implicitWidth + Kirigami.Units.largeSpacing
        implicitHeight: chip.implicitHeight + Kirigami.Units.smallSpacing
        radius: Kirigami.Units.cornerRadius
        color: Qt.rgba(root.accent.r, root.accent.g, root.accent.b, root.containsMouse ? 0.22 : 0.14)
        border.width: 1
        border.color: Qt.rgba(root.accent.r, root.accent.g, root.accent.b, 0.42)

        RowLayout {
            id: chip
            anchors.centerIn: parent
            spacing: Kirigami.Units.smallSpacing

            Rectangle {
                Layout.preferredWidth: Kirigami.Units.smallSpacing
                Layout.preferredHeight: Kirigami.Units.smallSpacing
                radius: width / 2
                color: root.accent
            }
            PlasmaComponents.Label {
                text: root.state.blocked ? "0:00" : root.state.shortTime(root.state.remainingSecs)
                font.weight: Font.DemiBold
                font.pointSize: Kirigami.Theme.smallFont.pointSize
            }
        }
    }

    PlasmaComponents.ToolTip {
        text: {
            if (!root.state.available) {
                return i18n("Screen time service is not running");
            }
            if (!root.state.enforcement) {
                return i18n("Screen time is being recorded, but not limited");
            }
            if (root.state.blocked) {
                return root.state.message;
            }
            return i18n("%1 left today", root.state.longTime(root.state.remainingSecs));
        }
    }
}
