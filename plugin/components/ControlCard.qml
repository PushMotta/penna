import QtQuick
import qs.Commons
import "../Model.js" as Model

// One control in the studio's left column, Fan Control's card (DESIGN.md §8.3): live RPM, duty, state and the curve it
// follows. Selecting a card puts its curve in the editor.
Rectangle {
  id: root

  property var control: null        // tick control
  property var tick: null
  property real nowSec: 0
  property bool selected: false
  property bool hasCursor: false
  property color foreground: Color.foreground
  property color accent: Color.accent
  property color urgent: Color.urgent
  property string fontFamily: Style.font.family
  property int tickMs: 1000
  property bool live: true
  property bool reducedMotion: false

  signal clicked()
  signal hovered(bool isHovered)

  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property bool hot: mouse.containsMouse || hasCursor
  readonly property var badge: Model.badge(control, tick, nowSec)
  readonly property bool handback: control ? control.state === "handback" : false
  readonly property real duty: control && control.duty !== null && control.duty !== undefined ? Number(control.duty) : 0

  property real glidedDuty: duty
  Behavior on glidedDuty { enabled: root.live && !root.reducedMotion; NumberAnimation { duration: root.tickMs; easing.type: Easing.Linear } }

  implicitHeight: column.implicitHeight + Style.spacing.lg * 2
  color: selected ? Util.alpha(foreground, 0.08) : hot ? Util.alpha(foreground, 0.08) : Util.alpha(foreground, 0.04)
  border.width: selected ? Style.space(2) : 1
  border.color: selected ? accent : hot ? Util.alpha(foreground, 0.25) : Util.alpha(foreground, 0.4)
  Behavior on color { ColorAnimation { duration: 120; easing.type: Easing.OutCubic } }
  Behavior on border.color { ColorAnimation { duration: 120; easing.type: Easing.OutCubic } }

  Column {
    id: column
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.top: parent.top
    anchors.margins: Style.spacing.lg
    spacing: Style.spacing.sm

    Item {
      width: parent.width
      implicitHeight: Math.max(nameText.implicitHeight, badgeItem.implicitHeight)
      Text {
        id: nameText
        anchors.left: parent.left
        anchors.right: badgeItem.left
        anchors.rightMargin: Style.spacing.md
        anchors.verticalCenter: parent.verticalCenter
        elide: Text.ElideRight
        textFormat: Text.PlainText
        text: root.control ? root.control.label + (Number(root.control.fans) > 1 ? " ×" + root.control.fans : "") : ""
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        font.bold: true
      }
      Badge {
        id: badgeItem
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        text: root.badge ? root.badge.text : ""
        tone: root.badge ? root.badge.tone : "dim"
        foreground: root.foreground
        accent: root.accent
        urgent: root.urgent
        fontFamily: root.fontFamily
      }
    }

    Item {
      width: parent.width
      implicitHeight: rpm.implicitHeight
      LiveNumber {
        id: rpm
        anchors.left: parent.left
        value: root.control && root.control.rpm !== null && root.control.rpm !== undefined ? Number(root.control.rpm) : NaN
        step: 10
        grouped: true
        suffix: " RPM"
        tickMs: root.tickMs
        live: root.live
        reducedMotion: root.reducedMotion
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
      }
      LiveNumber {
        anchors.right: parent.right
        value: root.handback || !root.control ? NaN : root.duty
        placeholder: "driver"
        step: 1
        suffix: " %"
        tickMs: root.tickMs
        live: root.live
        reducedMotion: root.reducedMotion
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
      }
    }

    Rectangle {
      width: parent.width
      height: Math.max(2, Style.space(2))
      color: Util.alpha(root.foreground, 0.12)
      Rectangle {
        width: parent.width * Math.max(0, Math.min(100, root.glidedDuty)) / 100
        height: parent.height
        color: root.control && Model.isAlert(root.control.state) ? root.urgent : root.foreground
        opacity: root.handback ? 0.3 : 0.8
      }
    }

    Item {
      width: parent.width
      implicitHeight: curveText.implicitHeight
      Text {
        id: curveText
        anchors.left: parent.left
        text: root.control ? (root.control.curve_label || (root.control.state === "firmware" ? "Firmware" : "")) : ""
        textFormat: Text.PlainText
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
      }
      Text {
        anchors.right: parent.right
        text: !root.control ? "" : root.control.trend === "rising" ? "↑ rising" : root.control.trend === "falling" ? "↓ falling" : ""
        textFormat: Text.PlainText
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
      }
    }
  }

  MouseArea {
    id: mouse
    anchors.fill: parent
    hoverEnabled: true
    cursorShape: Qt.PointingHandCursor
    onClicked: root.clicked()
    onContainsMouseChanged: root.hovered(containsMouse)
  }
}
