// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Layouts
import org.kde.plasma.components as PlasmaComponents
import org.kde.kirigami as Kirigami
import org.timebandits.screentime.private

/// The week: what has gone, and what the coming days hold.
///
/// With a daily budget each day carries its own allowance, so the figures are
/// facts. With a weekly budget there is no per-day allowance — that is the
/// point of it — so the strip shows the pot and, separately, what it works out
/// to per remaining day, labelled as the arithmetic it is. Inventing a daily
/// share would show a rule the policy does not contain.
ColumnLayout {
    id: root

    required property var fmt
    spacing: Kirigami.Units.smallSpacing

    readonly property bool weekly: ScreenTimeAgent.budgetKind === "weekly"
    readonly property int daysLeft: {
        let n = 0;
        for (const d of ScreenTimeAgent.week) {
            // Today counts: there is still time left in it.
            if (d.today) {
                n += 1;
            }
        }
        // Everything after today. The agent marks only today, so count forward
        // from its position rather than trusting a second flag.
        let seenToday = false;
        for (const d of ScreenTimeAgent.week) {
            if (seenToday) {
                n += 1;
            }
            if (d.today) {
                seenToday = true;
            }
        }
        return Math.max(n, 1);
    }

    PlasmaComponents.Label {
        text: i18n("THIS WEEK")
        color: Kirigami.Theme.disabledTextColor
        font.pointSize: Kirigami.Theme.smallFont.pointSize * 0.85
        font.letterSpacing: 1.1
        font.weight: Font.DemiBold
    }

    PlasmaComponents.Label {
        Layout.fillWidth: true
        visible: root.weekly && ScreenTimeAgent.weeklyRemainingSeconds >= 0
        wrapMode: Text.WordWrap
        font.pointSize: Kirigami.Theme.smallFont.pointSize
        text: i18nc("%1 time left in the week, %2 time per day, %3 number of days",
                    "%1 left this week — about %2 a day over %3 more days",
                    root.fmt.long(ScreenTimeAgent.weeklyRemainingSeconds),
                    root.fmt.long(Math.floor(ScreenTimeAgent.weeklyRemainingSeconds / root.daysLeft)),
                    root.daysLeft)
    }

    RowLayout {
        Layout.fillWidth: true
        Layout.topMargin: Kirigami.Units.smallSpacing
        spacing: Kirigami.Units.smallSpacing

        Repeater {
            model: ScreenTimeAgent.week
            delegate: ColumnLayout {
                required property var modelData
                Layout.fillWidth: true
                spacing: 3

                readonly property bool noComputer: modelData.allowanceSeconds === 0
                readonly property bool nothingToState: modelData.allowanceSeconds < 0

                PlasmaComponents.Label {
                    Layout.alignment: Qt.AlignHCenter
                    text: root.fmt.weekdayShort(modelData.weekday)
                    font.pointSize: Kirigami.Theme.smallFont.pointSize * 0.9
                    font.weight: modelData.today ? Font.Bold : Font.Normal
                    color: modelData.today ? Kirigami.Theme.textColor
                                           : Kirigami.Theme.disabledTextColor
                }

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
                            const allowance = modelData.allowanceSeconds;
                            if (allowance <= 0) {
                                return 0;
                            }
                            const share = Math.min(1, modelData.usedSeconds / allowance);
                            return share > 0 ? Math.max(2, (parent.height - 2) * share) : 0;
                        }
                        color: modelData.today ? Kirigami.Theme.highlightColor
                                               : Kirigami.Theme.disabledTextColor
                    }

                    /// A day with no allowance is not an empty bar. An empty bar
                    /// reads as "nothing used yet", which is the opposite.
                    Kirigami.Icon {
                        anchors.centerIn: parent
                        visible: parent.parent.noComputer
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
                        if (root.weekly || parent.noComputer) {
                            return "";
                        }
                        if (parent.nothingToState) {
                            return "∞";
                        }
                        const left = Math.max(0, modelData.allowanceSeconds - modelData.usedSeconds);
                        return root.fmt.short(left);
                    }
                }
            }
        }
    }
}
