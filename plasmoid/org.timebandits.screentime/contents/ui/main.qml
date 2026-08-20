// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import org.kde.plasma.plasmoid
import org.kde.kirigami as Kirigami

PlasmoidItem {
    id: root

    property ScreenTimeState screenTime: ScreenTimeState { }

    // Panel first: this is a glanceable counter, not a page to open.
    preferredRepresentation: compactRepresentation

    toolTipMainText: i18n("Screen Time")
    toolTipSubText: screenTime.available
                    ? (screenTime.enforcement
                       ? i18n("%1 left today", screenTime.longTime(screenTime.remainingSecs))
                       : i18n("Recording only"))
                    : i18n("Service not running")

    compactRepresentation: CompactRepresentation {
        state: root.screenTime
        onClicked: root.expanded = !root.expanded
    }

    fullRepresentation: FullRepresentation {
        state: root.screenTime
    }

    // The popup is worth a faster refresh than the panel; a counter someone is
    // looking at should not be twenty seconds stale.
    onExpandedChanged: screenTime.poll.interval = expanded ? 5000 : 20000
}
