import QtQuick
import qs.Commons

// One temperature in the popup: its name, a 60 s sparkline and whole degrees (DESIGN.md §8.2).
Item {
  id: root

  property string name: ""
  property string detail: ""
  property var values: []
  property real celsius: NaN
  property bool failed: false
  // Its device is asleep (§4.2): no number, and nothing wrong.
  property bool asleep: false
  property real lo: 20
  property real hi: 100
  property color foreground: Color.foreground
  property color urgent: Color.urgent
  property string fontFamily: Style.font.family
  property int tickMs: 1000
  property bool live: true
  property bool reducedMotion: false

  readonly property alias shownText: number.text
  readonly property color dim: Qt.darker(foreground, 1.4)

  function push() { spark.push() }

  implicitHeight: Math.max(nameText.implicitHeight, Style.space(20))

  Text {
    id: nameText
    anchors.left: parent.left
    anchors.right: spark.left
    anchors.rightMargin: Style.spacing.lg
    anchors.verticalCenter: parent.verticalCenter
    textFormat: Text.StyledText
    elide: Text.ElideRight
    text: root.detail !== "" ? root.name + " <font color=\"" + root.dim + "\">· " + root.detail + "</font>" : root.name
    color: root.foreground
    font.family: root.fontFamily
    font.pixelSize: Style.font.body
  }

  Sparkline {
    id: spark
    anchors.right: number.left
    anchors.rightMargin: Style.spacing.lg
    anchors.verticalCenter: parent.verticalCenter
    width: Style.space(150)
    height: Style.space(20)
    values: root.values
    lo: root.lo
    hi: root.hi
    window: 60
    tickMs: root.tickMs
    live: root.live
    reducedMotion: root.reducedMotion
    foreground: root.foreground
  }

  LiveNumber {
    id: number
    anchors.right: parent.right
    anchors.verticalCenter: parent.verticalCenter
    width: Style.space(56)
    horizontalAlignment: Text.AlignRight
    value: root.failed || root.asleep ? NaN : root.celsius
    placeholder: root.asleep ? "asleep" : "—"
    step: 1
    suffix: " °C"
    tickMs: root.tickMs
    live: root.live
    reducedMotion: root.reducedMotion
    color: root.failed ? root.urgent : root.asleep ? root.dim : root.foreground
    font.family: root.fontFamily
    font.pixelSize: Style.font.body
  }
}
