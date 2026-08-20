// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import org.kde.kirigami as Kirigami

// The remaining-time ring. A bar implies a finish line you are travelling
// towards; a ring reads as a budget being spent, which is what this is.
Item {
    id: root

    property real fraction: 0.5          // 0..1 of the allowance still left
    property color accent: Kirigami.Theme.highlightColor
    property string bigText: ""
    property string unitText: ""

    implicitWidth: Kirigami.Units.gridUnit * 8
    implicitHeight: Kirigami.Units.gridUnit * 8

    Canvas {
        id: canvas
        anchors.fill: parent
        // Repaint on every input that changes the drawing, or the ring keeps
        // the previous state's colour after a property change.
        onPaint: {
            const ctx = getContext("2d");
            const w = width, h = height;
            const cx = w / 2, cy = h / 2;
            const lw = Math.max(6, w * 0.075);
            const r = Math.min(cx, cy) - lw / 2 - 1;

            ctx.reset();
            ctx.lineWidth = lw;
            ctx.lineCap = "round";

            ctx.strokeStyle = Qt.rgba(Kirigami.Theme.textColor.r,
                                      Kirigami.Theme.textColor.g,
                                      Kirigami.Theme.textColor.b, 0.12);
            ctx.beginPath();
            ctx.arc(cx, cy, r, 0, Math.PI * 2);
            ctx.stroke();

            if (root.fraction > 0) {
                ctx.strokeStyle = root.accent;
                ctx.beginPath();
                ctx.arc(cx, cy, r, -Math.PI / 2,
                        -Math.PI / 2 + Math.PI * 2 * root.fraction);
                ctx.stroke();
            }
        }
        Component.onCompleted: requestPaint()
    }

    Connections {
        target: root
        function onFractionChanged() { canvas.requestPaint(); }
        function onAccentChanged() { canvas.requestPaint(); }
    }

    Column {
        anchors.centerIn: parent
        spacing: 0

        Text {
            anchors.horizontalCenter: parent.horizontalCenter
            text: root.bigText
            color: Kirigami.Theme.textColor
            font.family: Kirigami.Theme.defaultFont.family
            font.pixelSize: Math.round(root.height * 0.19)
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
