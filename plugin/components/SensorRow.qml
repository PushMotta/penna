import QtQuick
import qs.Commons
import "../Model.js" as Model

// One sensor in the studio's right column: five minutes on a fixed scale, and the reading to a tenth of a degree.
Column {
  id: root

  property string key: ""
  property string name: ""
  property string detail: ""
  property var values: []
  property real celsius: NaN
  // Its device is asleep (§4.2): no number, and nothing wrong.
  property bool asleep: false
  property real lo: 20
  property real hi: 100
  property color foreground: Color.foreground
  property string fontFamily: Style.font.family
  property int tickMs: 1000
  property bool live: true
  property bool reducedMotion: false

  readonly property color dim: Qt.darker(foreground, 1.4)

  function push() { spark.push() }

  spacing: Style.spacing.xs

  Item {
    width: parent.width
    implicitHeight: label.implicitHeight
    Text {
      id: label
      anchors.left: parent.left
      anchors.right: value.left
      anchors.rightMargin: Style.spacing.md
      elide: Text.ElideRight
      textFormat: Text.StyledText
      text: Model.dimmedLine(root.name, root.detail !== "" ? "· " + root.detail : "", root.dim, false)
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
    }
    LiveNumber {
      id: value
      anchors.right: parent.right
      value: root.asleep ? NaN : root.celsius
      placeholder: root.asleep ? "asleep" : "—"
      step: 0.1
      decimals: 1
      suffix: " °C"
      tickMs: root.tickMs
      live: root.live
      reducedMotion: root.reducedMotion
      color: root.asleep ? root.dim : root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
    }
  }

  Sparkline {
    id: spark
    width: parent.width
    height: Style.space(28)
    values: root.values
    lo: root.lo
    hi: root.hi
    window: 300
    tickMs: root.tickMs
    live: root.live
    reducedMotion: root.reducedMotion
    foreground: root.foreground
  }
}
