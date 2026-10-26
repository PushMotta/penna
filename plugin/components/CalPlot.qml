import QtQuick
import QtQuick.Shapes
import qs.Commons

// One fan's calibration as setup measures it (DESIGN.md §5.4, design review 03 call 6): duty → RPM on the fan's own scale,
// the measured points joined, and where it is now. Shapes, not Canvas (§8.6). A pump or chipset fan that isn't measured
// below some duty has that region shaded.
Item {
  id: root

  property var table: []             // [[duty, rpm]], ascending
  property real maxRpm: 1
  property real duty: NaN
  property real rpm: NaN
  property real lowest: NaN
  property color foreground: Color.foreground
  property color accent: Color.accent
  property string fontFamily: Style.font.family

  readonly property real axis: Style.space(11)
  readonly property real plotH: height - axis

  function xOf(d) { return Math.max(0, Math.min(100, d)) / 100 * width }
  function yOf(r) { return plotH - Math.max(0, Math.min(1, r / Math.max(1, maxRpm))) * (plotH - 2) - 1 }
  function screen(pairs) { return (pairs || []).map(function(p) { return Qt.point(root.xOf(p[0]), root.yOf(p[1])) }) }

  Rectangle {
    visible: !isNaN(root.lowest)
    x: 0
    width: root.xOf(root.lowest)
    height: root.plotH
    color: Util.alpha(root.foreground, 0.04)
  }

  Repeater {
    model: [0, 50, 100]
    Rectangle {
      required property var modelData
      x: Math.min(root.width - 1, root.xOf(modelData))
      width: 1
      height: root.plotH
      color: Util.alpha(root.foreground, 0.12)
    }
  }

  Shape {
    anchors.fill: parent
    preferredRendererType: Shape.CurveRenderer
    ShapePath {
      strokeColor: root.foreground
      strokeWidth: 1.5
      fillColor: "transparent"
      joinStyle: ShapePath.RoundJoin
      PathPolyline { path: root.screen(root.table) }
    }
  }

  Repeater {
    model: root.table
    Rectangle {
      required property var modelData
      width: Style.space(3)
      height: width
      radius: width / 2
      x: root.xOf(modelData[0]) - width / 2
      y: root.yOf(modelData[1]) - height / 2
      color: root.foreground
    }
  }

  Rectangle {
    visible: !isNaN(root.duty) && !isNaN(root.rpm)
    width: Style.space(6)
    height: width
    radius: width / 2
    x: root.xOf(root.duty) - width / 2
    y: root.yOf(root.rpm) - height / 2
    color: root.accent
  }

  Row {
    anchors.bottom: parent.bottom
    width: parent.width
    Repeater {
      model: ["0 %", "50 %", "100 %"]
      Text {
        textFormat: Text.PlainText
        required property var modelData
        required property int index
        width: root.width / 3
        horizontalAlignment: index === 0 ? Text.AlignLeft : index === 1 ? Text.AlignHCenter : Text.AlignRight
        text: modelData
        color: Qt.darker(root.foreground, 1.4)
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption * 0.85
      }
    }
  }
}
