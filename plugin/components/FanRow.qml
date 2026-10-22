import QtQuick
import qs.Commons
import qs.Ui
import "../Model.js" as Model

// One fan in the popup: RPM first, because it is what you hear, then duty (review call 3). The 2 px line under the row is
// its duty, gliding. A control that needs attention says why in words, under its name. A fan its BIOS or driver runs has no
// duty Penna can read, so it shows none (design review 03, call 3).
Item {
  id: root

  property var control: null
  property var tick: null
  property real nowSec: 0
  property bool canWrite: false
  property color foreground: Color.foreground
  property color accent: Color.accent
  property color urgent: Color.urgent
  property string fontFamily: Style.font.family
  property int tickMs: 1000
  property bool live: true
  property bool reducedMotion: false
  property bool hasCursor: false

  signal retryRequested(string key)
  signal hovered(bool isHovered)

  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property var badge: Model.badge(control, tick, nowSec)
  readonly property bool alerting: control ? Model.isAlert(control.state) : false
  readonly property bool handback: control ? control.state === "handback" : false
  readonly property bool hasDuty: !!control && control.duty !== null && control.duty !== undefined
  readonly property real duty: hasDuty ? Number(control.duty) : 0
  readonly property bool canRetry: canWrite && control && (control.state === "contested" || control.state === "fault")

  implicitHeight: column.implicitHeight

  property real glidedDuty: duty
  Behavior on glidedDuty { enabled: root.live && !root.reducedMotion; NumberAnimation { duration: root.tickMs; easing.type: Easing.Linear } }

  Column {
    id: column
    width: parent.width
    spacing: Style.spacing.xs

    Item {
      width: parent.width
      implicitHeight: Math.max(name.implicitHeight, badgeItem.implicitHeight) + Style.spacing.xs

      Text {
        id: name
        anchors.left: parent.left
        anchors.right: right.left
        anchors.rightMargin: Style.spacing.lg
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.StyledText
        elide: Text.ElideRight
        text: {
          if (!root.control) return ""
          var n = Math.max(1, Number(root.control.fans) || 0)
          return root.control.label + (n > 1 ? " <font color=\"" + root.dim + "\"><small>×" + n + "</small></font>" : "")
        }
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
      }

      Row {
        id: right
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.spacing.lg

        Badge {
          id: badgeItem
          anchors.verticalCenter: parent.verticalCenter
          text: root.badge ? root.badge.text : ""
          tone: root.badge ? root.badge.tone : "dim"
          foreground: root.foreground
          accent: root.accent
          urgent: root.urgent
          fontFamily: root.fontFamily
        }

        Text {
          anchors.verticalCenter: parent.verticalCenter
          text: root.control && root.control.curve_label ? root.control.curve_label : ""
          visible: text !== ""
          textFormat: Text.PlainText
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
        }

        LiveNumber {
          anchors.verticalCenter: parent.verticalCenter
          width: Style.space(76)
          horizontalAlignment: Text.AlignRight
          value: root.control && root.control.rpm !== null && root.control.rpm !== undefined ? Number(root.control.rpm) : NaN
          step: 10
          grouped: true
          suffix: " RPM"
          tickMs: root.tickMs
          live: root.live
          reducedMotion: root.reducedMotion
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
        }

        LiveNumber {
          anchors.verticalCenter: parent.verticalCenter
          width: Style.space(40)
          horizontalAlignment: Text.AlignRight
          value: root.handback || !root.hasDuty ? NaN : root.duty
          step: 1
          suffix: " %"
          tickMs: root.tickMs
          live: root.live
          reducedMotion: root.reducedMotion
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
        }
      }
    }

    Rectangle {
      width: parent.width
      height: Math.max(2, Style.space(2))
      color: Util.alpha(root.foreground, 0.06)
      Rectangle {
        width: parent.width * Math.max(0, Math.min(100, root.glidedDuty)) / 100
        height: parent.height
        color: root.alerting ? root.urgent : root.foreground
        opacity: root.handback ? 0.25 : 0.55
      }
    }

    Text {
      width: parent.width
      visible: root.alerting && root.control && !!root.control.reason
      text: root.control && root.control.reason ? String(root.control.reason) : ""
      textFormat: Text.PlainText
      wrapMode: Text.Wrap
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      lineHeight: 1.15
    }

    Button {
      visible: root.canRetry
      text: "Try again"
      fontSize: Style.font.caption
      bordered: true
      foreground: root.foreground
      fontFamily: root.fontFamily
      hasCursor: root.hasCursor
      onClicked: root.retryRequested(root.control.key)
      onHovered: function(h) { root.hovered(h) }
    }
  }
}
