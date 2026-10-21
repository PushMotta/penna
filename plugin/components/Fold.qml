import QtQuick
import qs.Commons

// A folded list of things Penna isn't running: headers the firmware still drives, devices it can't reach. Each says why.
// The lines are one text block, so a tick that re-sends the same lines rebuilds nothing.
Column {
  id: root

  property string title: ""
  property int count: 0
  property var lines: []
  property bool expanded: false
  property color foreground: Color.foreground
  property string fontFamily: Style.font.family

  readonly property color dim: Qt.darker(foreground, 1.4)

  visible: count > 0
  spacing: Style.spacing.xs

  Item {
    width: parent.width
    implicitHeight: header.implicitHeight + Style.spacing.xs * 2

    Text {
      id: header
      anchors.left: parent.left
      anchors.verticalCenter: parent.verticalCenter
      text: (root.expanded ? "▾  " : "▸  ") + root.title + " (" + root.count + ")"
      textFormat: Text.PlainText
      color: mouse.containsMouse ? root.foreground : root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
    }

    MouseArea {
      id: mouse
      anchors.fill: parent
      hoverEnabled: true
      cursorShape: Qt.PointingHandCursor
      onClicked: root.expanded = !root.expanded
    }
  }

  Text {
    visible: root.expanded
    width: root.width
    leftPadding: Style.space(18)
    text: (root.lines || []).join("\n")
    textFormat: Text.PlainText
    wrapMode: Text.Wrap
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    lineHeight: 1.2
  }
}
