// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Layouts
import org.kde.plasma.components as PlasmaComponents
import org.kde.kirigami as Kirigami

/// The week at a glance: what is left today, and what each of the coming days
/// holds.
///
/// With a daily budget each day carries its own allowance, so the figures are
/// facts. With a weekly budget there is no per-day allowance — that is the
/// point of it — so showing one would be inventing a number the rules do not
/// contain. The strip then shows the week's remainder and, separately, what it
/// works out to per remaining day, labelled as the suggestion it is.
ColumnLayout {
    id: root

    required property var state
    spacing: Kirigami.Units.smallSpacing

    readonly property bool weekly: state.budgetKind === "weekly"
    readonly property int daysLeft: {
        let n = 0;
        for (const d of state.week) {
            if (d.today || d.future) {
                n += 1;
            }
        }
        return Math.max(n, 1);
    }

    PlasmaComponents.Label {
        text: root.weekly ? i18n("THIS WEEK") : i18n("THIS WEEK")
        color: Kirigami.Theme.disabledTextColor
        font.pointSize: Kirigami.Theme.smallFont.pointSize * 0.85
        font.letterSpacing: 1.1
        font.weight: Font.DemiBold
    }

    // With a weekly budget the pot is the headline; the per-day figure below
    // it is arithmetic, not allocation, and says so.
    PlasmaComponents.Label {
        Layout.fillWidth: true
        visible: root.weekly && root.state.weeklyRemainingSecs >= 0
        wrapMode: Text.WordWrap
        font.pointSize: Kirigami.Theme.smallFont.pointSize
        text: i18nc(
            "%1 is a duration left in the week, %2 a duration per day, %3 a count of days",
            "%1 left this week — about %2 a day across %3 more days",
            root.state.longTime(root.state.weeklyRemainingSecs),
            root.state.longTime(Math.floor(root.state.weeklyRemainingSecs / root.daysLeft)),
            root.daysLeft)
    }

    RowLayout {
        Layout.fillWidth: true
        Layout.topMargin: Kirigami.Units.smallSpacing
        spacing: Kirigami.Units.smallSpacing

        Repeater {
            model: root.state.week
            delegate: ColumnLayout {
                required property var modelData
                Layout.fillWidth: true
                spacing: 3

                PlasmaComponents.Label {
                    Layout.alignment: Qt.AlignHCenter
                    text: modelData.short_name || ""
                    font.pointSize: Kirigami.Theme.smallFont.pointSize * 0.9
                    font.weight: modelData.today ? Font.Bold : Font.Normal
                    color: modelData.today ? Kirigami.Theme.textColor
                                           : Kirigami.Theme.disabledTextColor
                }

                // A column per day: filled portion is time already spent.
                Rectangle {
                    Layout.fillWidth: true
                    Layout.preferredHeight: Kirigami.Units.gridUnit * 2
                    radius: 3
                    color: Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g,
                                   Kirigami.Theme.textColor.b, 0.08)
                    border.width: modelData.today ? 1 : 0
                    border.color: Kirigami.Theme.highlightColor

                    Rectangle {
                        anchors { left: parent.left; right: parent.right; bottom: parent.bottom }
                        anchors.margins: 1
                        radius: 2
                        height: {
                            const allowance = modelData.allowance_secs;
                            if (!allowance || allowance <= 0) {
                                return 0;
                            }
                            const share = Math.min(1, (modelData.used_secs || 0) / allowance);
                            return Math.max(share > 0 ? 2 : 0, (parent.height - 2) * share);
                        }
                        color: modelData.today ? Kirigami.Theme.highlightColor
                                               : Kirigami.Theme.disabledTextColor
                    }

                    // A day with no allowance is not an empty bar — an empty
                    // bar reads as "nothing used yet", which is the opposite.
                    Kirigami.Icon {
                        anchors.centerIn: parent
                        visible: modelData.allowance_secs === 0
                        source: "dialog-cancel"
                        width: Kirigami.Units.iconSizes.small
                        height: width
                        color: Kirigami.Theme.disabledTextColor
                        isMask: true
                    }
                }

                PlasmaComponents.Label {
                    Layout.alignment: Qt.AlignHCenter
                    font.pointSize: Kirigami.Theme.smallFont.pointSize * 0.85
                    color: Kirigami.Theme.disabledTextColor
                    text: {
                        if (root.weekly) {
                            return "";
                        }
                        const a = modelData.allowance_secs;
                        if (a === 0) {
                            return "";
                        }
                        if (a === null || a === undefined) {
                            return "∞";
                        }
                        const left = Math.max(0, a - (modelData.used_secs || 0));
                        return root.state.shortTime(left);
                    }
                }
            }
        }
    }
}
