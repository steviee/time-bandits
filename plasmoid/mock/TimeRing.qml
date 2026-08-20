// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Shapes
import org.kde.kirigami as Kirigami

/// The remaining-time ring.
///
/// A bar implies a finish line you are travelling towards; a ring reads as a
/// budget being spent, which is what this is.
///
/// Drawn with Shapes rather than Canvas. Canvas paints imperatively, once, when
/// something asks it to — and inside a Plasma popup that first paint lands
/// before the layout has sized anything, leaving no ring and no error to say
/// why. A declarative arc has no such moment to miss.
Item {
    id: root

    /// How much of the allowance is still left, 0..1.
    property real fraction: 0.5
    property color accent: Kirigami.Theme.highlightColor
    property string bigText: ""
    property string unitText: ""

    implicitWidth: Kirigami.Units.gridUnit * 8
    implicitHeight: Kirigami.Units.gridUnit * 8

    readonly property real stroke: Math.max(6, Math.min(width, height) * 0.075)
    readonly property real radius: Math.min(width, height) / 2 - stroke / 2 - 1

    Shape {
        anchors.fill: parent
        preferredRendererType: Shape.CurveRenderer

        // The track: what the whole allowance would look like.
        ShapePath {
            strokeColor: Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g,
                                 Kirigami.Theme.textColor.b, 0.12)
            strokeWidth: root.stroke
            fillColor: "transparent"
            capStyle: ShapePath.RoundCap
            PathAngleArc {
                centerX: root.width / 2
                centerY: root.height / 2
                radiusX: root.radius
                radiusY: root.radius
                startAngle: -90
                sweepAngle: 360
            }
        }

        // What is left of it. Starts at twelve o'clock and runs clockwise.
        ShapePath {
            strokeColor: root.accent
            strokeWidth: root.stroke
            fillColor: "transparent"
            capStyle: ShapePath.RoundCap
            PathAngleArc {
                centerX: root.width / 2
                centerY: root.height / 2
                radiusX: root.radius
                radiusY: root.radius
                startAngle: -90
                sweepAngle: 360 * Math.max(0, Math.min(1, root.fraction))
            }
        }
    }

    Column {
        anchors.centerIn: parent
        spacing: 0

        Text {
            anchors.horizontalCenter: parent.horizontalCenter
            text: root.bigText
            color: Kirigami.Theme.textColor
            font.family: Kirigami.Theme.defaultFont.family
            font.pixelSize: Math.round(root.height * 0.17)
            font.weight: Font.Bold
            font.letterSpacing: -0.5
        }
        Text {
            anchors.horizontalCenter: parent.horizontalCenter
            text: root.unitText
            color: Kirigami.Theme.disabledTextColor
            font.family: Kirigami.Theme.defaultFont.family
            font.pixelSize: Math.round(root.height * 0.082)
            font.letterSpacing: 0.6
        }
    }
}
