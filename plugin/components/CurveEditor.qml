import QtQuick
import QtQuick.Shapes
import qs.Commons
import "../Model.js" as Model

// The curve editor (DESIGN.md §8.3). X is °C, Y is duty %, and the control's calibrated RPM sits on the right. Handles
// snap to whole °C and %, and a graph's neighbours are pushed so it never dips. The marker rides the curve with a 10 s
// trail; inside the shaded band it sits off the curve, which is the fan being held.
//
// While a handle is held, the drawn curve is local: the daemon's echo of the draft never moves a point under the hand.
Item {
  id: root

  property var curve: null          // studio body curve (the draft)
  property var control: null        // studio body control: floor, stop, nvml, calibration
  property var live: null           // tick curve: smoothed, out, held, hold_until
  property var marks: []            // a generated curve's anchors, [{ t, label }] (Model.anchorMarks)
  property int tickMs: 1000
  property bool reducedMotion: false
  property bool running: true
  property bool editable: true
  property bool keyboardActive: false
  property color foreground: Color.foreground
  property color accent: Color.accent
  property string fontFamily: Style.font.family

  signal edited(var curve, bool final)

  readonly property string kind: curve ? String(curve.kind) : ""
  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property bool animate: running && !reducedMotion

  readonly property real padL: Style.space(36)
  readonly property real padR: control && control.calibration ? Style.space(52) : Style.space(16)
  readonly property real padT: Style.space(20)
  readonly property real padB: Style.space(24)
  readonly property real plotW: Math.max(1, width - padL - padR)
  readonly property real plotH: Math.max(1, height - padT - padB)

  function sx(t) { return padL + (t - Model.T_MIN) / (Model.T_MAX - Model.T_MIN) * plotW }
  function sy(d) { return padT + plotH - d / 100 * plotH }
  function tAt(x) { return Model.T_MIN + (x - padL) / plotW * (Model.T_MAX - Model.T_MIN) }
  function dAt(y) { return (padT + plotH - y) / plotH * 100 }
  function toScreen(pairs) { return pairs.map(function(p) { return Qt.point(root.sx(p[0]), root.sy(p[1])) }) }

  // ---------------------------------------------------------- handles
  property var handles: []
  property int selected: 0
  property int dragging: -1
  property int hovering: -1
  property point pressAt: Qt.point(0, 0)
  property bool moved: false

  function syncFromCurve() {
    if (dragging >= 0) return
    handles = Model.handlesOf(curve)
    selected = Math.max(0, Math.min(selected, handles.length - 1))
  }
  onCurveChanged: syncFromCurve()
  Component.onCompleted: syncFromCurve()

  function commit(next, final) {
    handles = next
    root.edited(Model.curveWith(curve, next), final)
  }

  function nudge(dt, dd) {
    if (!editable || !handles.length) return
    var h = handles[selected]
    commit(Model.moveHandle(kind, handles, selected, h[0] + dt, h[1] + dd), true)
  }
  function selectBy(step) { if (handles.length) selected = Math.max(0, Math.min(handles.length - 1, selected + step)) }
  function addHandle() {
    if (!editable || kind !== "graph") return
    var r = Model.addPoint(handles, selected)
    if (r.points.length !== handles.length) {
      selected = r.index
      commit(r.points, true)
    }
  }
  function removeHandle() {
    if (!editable || kind !== "graph") return
    var r = Model.deletePoint(handles, selected)
    if (r.points.length !== handles.length) {
      selected = r.index
      commit(r.points, true)
    }
  }

  function nearest(x, y) {
    var best = -1
    var bestD = Style.space(14)
    for (var i = 0; i < handles.length; i++) {
      var d = Math.hypot(sx(handles[i][0]) - x, sy(handles[i][1]) - y)
      if (d <= bestD) {
        best = i
        bestD = d
      }
    }
    return best
  }

  readonly property var pathPairs: Model.pathOf(kind, handles)
  readonly property var shadePairs: Model.shadeOf(curve, handles)

  // ---------------------------------------------------------- the live marker
  // The temperature and the hold offset glide linearly over a tick; the marker is drawn at f(T) + offset, so it rides
  // the curve instead of cutting across it, and leaves it only while something holds the output (§8.6).
  property bool primed: false
  property real gT: 0
  property real gOff: 0
  property real gOut: 0
  Behavior on gT { enabled: root.animate && root.primed; NumberAnimation { duration: root.tickMs; easing.type: Easing.Linear } }
  Behavior on gOff { enabled: root.animate && root.primed; NumberAnimation { duration: root.tickMs; easing.type: Easing.Linear } }
  Behavior on gOut { enabled: root.animate && root.primed; NumberAnimation { duration: root.tickMs; easing.type: Easing.Linear } }

  readonly property bool hasMarker: live !== null && live !== undefined && live.out !== null && live.out !== undefined
                                    && (kind === "flat" || (live.smoothed !== null && live.smoothed !== undefined))

  function feed() {
    if (!hasMarker) {
      primed = false
      return
    }
    var t = kind === "flat" ? 60 : Number(live.smoothed)
    var out = Number(live.out)
    var off = kind === "graph" ? out - Model.evalPoints(Model.handlesOf(curve), t) : 0
    if (!primed) {
      gT = t
      gOff = off
      gOut = out
      primed = true
    } else {
      gT = t
      gOff = off
      gOut = out
    }
  }
  onLiveChanged: feed()
  onRunningChanged: if (running) {
    primed = false
    trail = []
    feed()
  }

  readonly property real markT: Model.clamp(gT, Model.T_MIN, Model.T_MAX)
  readonly property real markD: Model.clamp(kind === "graph" ? Model.evalPoints(handles, markT) + gOff : gOut, 0, 100)

  property var trail: []
  Timer {
    interval: 250
    repeat: true
    running: root.running && root.animate && root.hasMarker
    onTriggered: {
      var now = Date.now()
      var next = root.trail.filter(function(p) { return now - p.t < 10000 })
      next.push({ x: root.sx(root.markT), y: root.sy(root.markD), t: now })
      if (next.length > 40) next.splice(0, next.length - 40)
      root.trail = next
    }
  }

  // ---------------------------------------------------------- grid and axes
  Repeater {
    model: 9
    Rectangle {
      required property int index
      readonly property int t: 20 + index * 10
      x: Math.round(root.sx(t))
      y: root.padT
      width: 1
      height: root.plotH
      color: Util.alpha(root.foreground, 0.08)
    }
  }
  Repeater {
    model: 11
    Rectangle {
      required property int index
      x: root.padL
      y: Math.round(root.sy(index * 10))
      width: root.plotW
      height: 1
      color: Util.alpha(root.foreground, 0.08)
    }
  }
  Repeater {
    model: 9
    Text {
      required property int index
      readonly property int t: 20 + index * 10
      text: t === 100 ? "100 °C" : String(t)
      x: Math.round(root.sx(t) - (t === 100 ? implicitWidth - Style.space(8) : implicitWidth / 2))
      y: root.padT + root.plotH + Style.spacing.sm
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
    }
  }
  Repeater {
    model: 6
    Text {
      required property int index
      readonly property int d: index * 20
      text: String(d)
      x: root.padL - implicitWidth - Style.spacing.md
      y: Math.round(root.sy(d) - implicitHeight / 2)
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
    }
  }
  Text {
    text: "%"
    x: root.padL - implicitWidth - Style.spacing.md
    y: 0
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
  }
  Repeater {
    model: root.control && root.control.calibration ? 5 : 0
    Text {
      required property int index
      readonly property int d: 20 + index * 20
      text: Model.formatNumber(Model.rpmAt(root.control.calibration, d), 10, 0, true)
      x: root.padL + root.plotW + Style.spacing.md
      y: Math.round(root.sy(d) - implicitHeight / 2)
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      font.features: ({ "tnum": 1 })
    }
  }
  Text {
    visible: !!(root.control && root.control.calibration)
    text: "RPM"
    x: root.padL + root.plotW + Style.spacing.md
    y: 0
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
  }

  // A stepped control's levels (§3.5): the curve's duty is rounded up to the next of these.
  Repeater {
    model: root.control && root.control.levels ? root.control.levels - 1 : 0
    Rectangle {
      required property int index
      x: root.padL
      y: Math.round(root.sy(Model.levelDuty(root.control.levels, index + 1)))
      width: root.plotW
      height: 1
      color: Util.alpha(root.accent, 0.18)
    }
  }

  // ---------------------------------------------------------- limits
  // NVIDIA: below the floor the driver runs the fan (§4.5). Motherboard: the floor, and where the fan stops.
  readonly property real floorDuty: control ? (control.nvml ? Model.nvmlFloor(control) : Number(control.floor)) : -1
  readonly property real stopDuty: control && !control.nvml && control.stop !== null && control.stop !== undefined ? Number(control.stop) : -1

  Rectangle {
    visible: !!(root.control && root.control.nvml)
    x: root.padL
    y: root.sy(root.floorDuty)
    width: root.plotW
    height: root.sy(0) - root.sy(root.floorDuty)
    color: Util.alpha(root.foreground, 0.05)
  }

  Shape {
    anchors.fill: parent
    visible: root.floorDuty >= 0
    ShapePath {
      strokeColor: Util.alpha(root.foreground, 0.4)
      strokeWidth: 1
      strokeStyle: ShapePath.DashLine
      dashPattern: [4, 4]
      fillColor: "transparent"
      startX: root.padL; startY: Math.round(root.sy(root.floorDuty)) + 0.5
      PathLine { x: root.padL + root.plotW; y: Math.round(root.sy(root.floorDuty)) + 0.5 }
    }
    ShapePath {
      strokeColor: root.stopDuty >= 0 ? Util.alpha(root.foreground, 0.4) : "transparent"
      strokeWidth: 1
      strokeStyle: ShapePath.DashLine
      dashPattern: [2, 4]
      fillColor: "transparent"
      startX: root.padL; startY: Math.round(root.sy(Math.max(0, root.stopDuty))) + 0.5
      PathLine { x: root.padL + root.plotW; y: Math.round(root.sy(Math.max(0, root.stopDuty))) + 0.5 }
    }
  }
  Text {
    visible: root.floorDuty >= 0
    text: root.control && root.control.nvml ? "below " + Math.round(root.floorDuty) + " %: the driver runs this fan" : "min " + Math.round(root.floorDuty) + " %"
    x: root.control && root.control.nvml ? root.padL + Style.spacing.md : root.padL + root.plotW - implicitWidth - Style.spacing.md
    y: root.control && root.control.nvml ? root.sy(root.floorDuty) + Style.spacing.sm : root.sy(root.floorDuty) - implicitHeight - Style.spacing.xs
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
  }
  Text {
    visible: root.stopDuty >= 0
    text: "stops below " + Math.round(root.stopDuty) + " %"
    x: root.padL + root.plotW - implicitWidth - Style.spacing.md
    y: root.sy(root.stopDuty) + Style.spacing.xs
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
  }

  // ---------------------------------------------------------- anchors
  // Where setup placed a generated curve from (§5.5): its source at rest, the target, the limit. A label that would run into
  // the one before goes a line up.
  readonly property var anchorRows: {
    var rows = []
    var right = -1e9
    var row = 0
    for (var i = 0; i < marks.length; i++) {
      var x = sx(marks[i].t) + Style.spacing.xs
      row = x < right + Style.spacing.md ? 1 - row : 0
      rows.push(row)
      right = x + marks[i].label.length * Style.font.caption * 0.6
    }
    return rows
  }
  Repeater {
    model: root.marks.length
    Item {
      required property int index
      readonly property var a: root.marks[index]
      Shape {
        width: root.width
        height: root.height
        ShapePath {
          strokeColor: Util.alpha(root.accent, 0.6)
          strokeWidth: 1
          strokeStyle: ShapePath.DashLine
          dashPattern: [3, 3]
          fillColor: "transparent"
          startX: Math.round(root.sx(a.t)) + 0.5; startY: root.padT
          PathLine { x: Math.round(root.sx(a.t)) + 0.5; y: root.padT + root.plotH }
        }
      }
      Text {
        text: a.label
        x: Math.round(root.sx(a.t)) + Style.spacing.xs
        // Along the bottom, where a curve is low at idle and long gone up by the limit.
        y: root.padT + root.plotH - (root.anchorRows[index] + 1) * (implicitHeight + Style.spacing.xs)
        color: root.accent
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
      }
    }
  }

  // ---------------------------------------------------------- shade and curve
  Shape {
    anchors.fill: parent
    preferredRendererType: Shape.CurveRenderer
    ShapePath {
      strokeWidth: 0
      strokeColor: "transparent"
      fillColor: Util.alpha(root.foreground, 0.09)
      PathPolyline { path: root.toScreen(root.shadePairs) }
    }
    ShapePath {
      strokeWidth: Style.space(2)
      strokeColor: root.foreground
      fillColor: "transparent"
      joinStyle: ShapePath.RoundJoin
      capStyle: ShapePath.RoundCap
      PathPolyline { path: root.toScreen(root.pathPairs) }
    }
  }

  // ---------------------------------------------------------- marker, crosshair, trail
  Shape {
    anchors.fill: parent
    visible: root.hasMarker
    opacity: 0.55
    ShapePath {
      strokeColor: root.accent
      strokeWidth: 1
      strokeStyle: ShapePath.DashLine
      dashPattern: [2, 3]
      fillColor: "transparent"
      startX: root.sx(root.markT); startY: root.sy(root.markD)
      PathLine { x: root.sx(root.markT); y: root.padT + root.plotH }
    }
    ShapePath {
      strokeColor: root.accent
      strokeWidth: 1
      strokeStyle: ShapePath.DashLine
      dashPattern: [2, 3]
      fillColor: "transparent"
      startX: root.padL; startY: root.sy(root.markD)
      PathLine { x: root.sx(root.markT); y: root.sy(root.markD) }
    }
  }

  Repeater {
    model: 40
    Rectangle {
      required property int index
      readonly property var p: root.trail.length > index ? root.trail[root.trail.length - 1 - index] : null
      visible: p !== null && root.animate
      width: Style.space(4)
      height: width
      radius: width / 2
      color: root.accent
      x: p ? p.x - width / 2 : 0
      y: p ? p.y - height / 2 : 0
      opacity: p ? 0.38 * Math.max(0, 1 - (Date.now() - p.t) / 10000) : 0
    }
  }

  Rectangle {
    visible: root.hasMarker
    width: Style.space(18)
    height: width
    radius: width / 2
    color: "transparent"
    border.width: 1
    border.color: Util.alpha(root.accent, 0.5)
    x: root.sx(root.markT) - width / 2
    y: root.sy(root.markD) - height / 2
  }
  Rectangle {
    visible: root.hasMarker
    width: Style.space(10)
    height: width
    radius: width / 2
    color: root.accent
    border.width: Style.space(2)
    border.color: Color.popups.background
    x: root.sx(root.markT) - width / 2
    y: root.sy(root.markD) - height / 2
  }

  // ---------------------------------------------------------- handles
  Repeater {
    model: root.handles.length
    Rectangle {
      required property int index
      readonly property var h: root.handles[index] || [0, 0]
      readonly property bool isSelected: index === root.selected && (root.keyboardActive || root.dragging === index)
      readonly property bool isHot: index === root.hovering || index === root.dragging
      width: isSelected || isHot ? Style.space(13) : Style.space(10)
      height: width
      radius: width / 2
      color: Color.popups.background
      border.width: Style.space(2)
      border.color: isSelected ? root.accent : root.foreground
      x: root.sx(h[0]) - width / 2
      y: root.sy(h[1]) - height / 2
      Behavior on width { enabled: !root.reducedMotion; NumberAnimation { duration: 120; easing.type: Easing.OutCubic } }
    }
  }

  // ---------------------------------------------------------- readout
  readonly property int readoutIndex: dragging >= 0 ? dragging : hovering >= 0 ? hovering : (keyboardActive ? selected : -1)
  Rectangle {
    id: readoutBox
    readonly property var h: root.readoutIndex >= 0 && root.handles[root.readoutIndex] ? root.handles[root.readoutIndex] : null
    visible: h !== null
    width: readoutText.implicitWidth + Style.spacing.lg * 2
    height: readoutText.implicitHeight + Style.spacing.sm * 2
    color: Color.popups.background
    border.width: 1
    border.color: Util.alpha(root.foreground, 0.4)
    // Beside the handle, never on it: flip left near the right edge, below near the top.
    x: !h ? 0 : (root.sx(h[0]) + Style.space(14) + width > root.width ? root.sx(h[0]) - Style.space(14) - width : root.sx(h[0]) + Style.space(14))
    y: !h ? 0 : (root.sy(h[1]) - Style.space(14) - height < 0 ? root.sy(h[1]) + Style.space(14) : root.sy(h[1]) - Style.space(14) - height)
    Text {
      id: readoutText
      anchors.centerIn: parent
      text: readoutBox.h ? (root.kind === "trigger" ? (root.readoutIndex === 0 ? "off " : "on ") : "") + Model.readout(readoutBox.h[0], readoutBox.h[1], root.control) : ""
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      font.features: ({ "tnum": 1 })
    }
  }

  // ---------------------------------------------------------- pointer
  // A press only becomes a drag after 4 px of travel, so clicking a handle to select it does not also move it.
  MouseArea {
    anchors.fill: parent
    hoverEnabled: true
    enabled: root.editable
    cursorShape: root.dragging >= 0 ? Qt.ClosedHandCursor : root.hovering >= 0 ? Qt.OpenHandCursor : Qt.ArrowCursor
    onPressed: function(mouse) {
      var i = root.nearest(mouse.x, mouse.y)
      if (i < 0) return
      root.selected = i
      root.dragging = i
      root.pressAt = Qt.point(mouse.x, mouse.y)
      root.moved = false
    }
    onPositionChanged: function(mouse) {
      if (root.dragging < 0) {
        root.hovering = root.nearest(mouse.x, mouse.y)
        return
      }
      if (!root.moved && Math.hypot(mouse.x - root.pressAt.x, mouse.y - root.pressAt.y) < Style.space(4)) return
      root.moved = true
      root.commit(Model.moveHandle(root.kind, root.handles, root.dragging, root.tAt(mouse.x), root.dAt(mouse.y)), false)
    }
    onReleased: {
      if (root.dragging >= 0 && root.moved) root.edited(Model.curveWith(root.curve, root.handles), true)
      root.dragging = -1
    }
    onExited: if (root.dragging < 0) root.hovering = -1
  }
}
