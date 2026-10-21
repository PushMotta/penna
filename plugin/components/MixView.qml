import QtQuick
import qs.Commons

// A Mix curve: its inputs and what they give, with the one driving the fans highlighted (§4.3). The larger input wins
// for `max`, the smaller for `min`; `average` highlights nothing.
Column {
  id: root

  property var curve: null          // studio body curve
  property var bodyCurves: []       // studio body curves, for labels
  property var liveCurves: []       // tick curves, for outputs
  property var usedBy: []           // labels of the controls that follow it
  property color foreground: Color.foreground
  property string fontFamily: Style.font.family
  property int tickMs: 1000
  property bool live: true
  property bool reducedMotion: false

  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property string op: curve && curve.mix ? String(curve.mix.op) : "max"
  readonly property var inputs: curve && curve.mix ? curve.mix.of : []

  function outOf(id) {
    for (var i = 0; i < liveCurves.length; i++) if (liveCurves[i].id === id) return liveCurves[i].out
    return null
  }
  function labelOf(id) {
    for (var i = 0; i < bodyCurves.length; i++) if (bodyCurves[i].id === id) return bodyCurves[i].label
    return id
  }
  readonly property string winner: {
    if (op === "average") return ""
    var best = ""
    var bestV = op === "min" ? Infinity : -Infinity
    for (var i = 0; i < inputs.length; i++) {
      var v = outOf(inputs[i])
      if (v === null || v === undefined) continue
      if ((op === "min" && v < bestV) || (op !== "min" && v > bestV)) {
        bestV = v
        best = inputs[i]
      }
    }
    return best
  }

  spacing: Style.spacing.md

  Text {
    text: (root.op === "max" ? "THE LARGER OF" : root.op === "min" ? "THE SMALLER OF" : "THE AVERAGE OF")
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    font.bold: true
  }

  Repeater {
    model: root.inputs.concat([root.curve ? root.curve.id : ""])
    Rectangle {
      required property var modelData
      required property int index
      readonly property bool isOutput: index === root.inputs.length
      readonly property real value: Number(root.outOf(modelData)) || 0
      property real glided: value
      Behavior on glided { enabled: root.live && !root.reducedMotion; NumberAnimation { duration: root.tickMs; easing.type: Easing.Linear } }
      width: root.width
      height: Style.space(30)
      color: !isOutput && modelData === root.winner ? Util.alpha(root.foreground, 0.12) : "transparent"
      Behavior on color { ColorAnimation { duration: 140; easing.type: Easing.OutCubic } }

      Text {
        id: rowLabel
        anchors.left: parent.left
        anchors.leftMargin: Style.spacing.md
        anchors.verticalCenter: parent.verticalCenter
        width: Style.space(120)
        elide: Text.ElideRight
        text: isOutput ? "Output" : root.labelOf(modelData)
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        font.bold: isOutput
      }
      Rectangle {
        anchors.left: rowLabel.right
        anchors.right: rowValue.left
        anchors.margins: Style.spacing.lg
        anchors.verticalCenter: parent.verticalCenter
        height: Style.space(6)
        color: Util.alpha(root.foreground, 0.06)
        Rectangle {
          width: parent.width * Math.max(0, Math.min(100, glided)) / 100
          height: parent.height
          color: root.foreground
          opacity: isOutput ? 0.9 : 0.6
        }
      }
      LiveNumber {
        id: rowValue
        anchors.right: parent.right
        anchors.rightMargin: Style.spacing.md
        anchors.verticalCenter: parent.verticalCenter
        width: Style.space(48)
        horizontalAlignment: Text.AlignRight
        value: parent.value
        suffix: " %"
        tickMs: root.tickMs
        live: root.live
        reducedMotion: root.reducedMotion
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        font.bold: isOutput
      }
    }
  }

  Text {
    width: root.width
    wrapMode: Text.Wrap
    text: root.usedBy.length ? "Used by " + root.usedBy.join(", ") + ". The highlighted input is driving the fans now." : "No control follows this curve yet."
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
  }
}
