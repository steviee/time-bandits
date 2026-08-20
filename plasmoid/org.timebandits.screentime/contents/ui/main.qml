// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import org.kde.plasma.plasmoid
import org.timebandits.screentime.private

PlasmoidItem {
    id: root

    /// Formatting lives in an object rather than a JavaScript library because
    /// every string in it goes through i18n(), which a `pragma library` cannot
    /// reach.
    readonly property Formatting fmt: Formatting { }

    /// A glanceable counter, not a page to open.
    preferredRepresentation: compactRepresentation

    toolTipMainText: i18n("Screen Time")
    toolTipSubText: ScreenTimeAgent.available
                    ? (ScreenTimeAgent.enforcement
                       ? i18n("%1 left today", fmt.long(ScreenTimeAgent.remainingSeconds))
                       : i18n("Recording only"))
                    : i18n("Service not running")

    compactRepresentation: CompactRepresentation {
        fmt: root.fmt
        onClicked: root.expanded = !root.expanded
    }

    fullRepresentation: FullRepresentation {
        fmt: root.fmt
    }
}
