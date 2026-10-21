import QtQuick
import qs.Commons

// A row's state in one word (review section F). The word carries the meaning; the tone only reinforces it.
Rectangle {
  id: root

  property string text: ""
  property string tone: "dim"
  property color foreground: Color.foreground
  property color accent: Color.accent
  property color urgent: Color.urgent
  property string fontFamily: Style.font.family

  readonly property color ink: tone === "urgent" ? urgent : tone === "accent" ? accent : Qt.darker(foreground, 1.4)

  visible: text !== ""
  implicitWidth: label.implicitWidth + Style.spacing.md * 2
  implicitHeight: label.implicitHeight + Style.spacing.xxs * 2
  color: "transparent"
  border.width: Math.max(1, Style.spacing.hairline)
  border.color: ink
  Behavior on border.color { ColorAnimation { duration: 140; easing.type: Easing.OutCubic } }

  Text {
    id: label
    anchors.centerIn: parent
    text: root.text
    textFormat: Text.PlainText
    color: root.ink
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    font.bold: true
    Behavior on color { ColorAnimation { duration: 140; easing.type: Easing.OutCubic } }
  }
}
