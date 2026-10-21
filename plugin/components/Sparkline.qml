import QtQuick
import QtQuick.Shapes
import qs.Commons

// A sensor's recent past on a fixed scale (DESIGN.md §8.6). The line is rebuilt once a tick and slides by a transform at
// display rate; only the newest segment moves its end each frame, so no frame rebuilds the whole line. Fixed scales, so
// nothing rescales or jumps.
Item {
  id: root

  // Oldest first, numbers or null. The service appends to it in place and the owner calls push() once per tick.
  property var values: []
  property real lo: 20
  property real hi: 100
  property int window: 60
  property int tickMs: 1000
  property bool live: true
  property bool reducedMotion: false
  property color foreground: Color.foreground

  readonly property bool animate: live && !reducedMotion
  readonly property real dx: width / Math.max(1, window - 1)
  property real phase: 1
  property int lastIndex: 0
  property real lastValue: NaN
  property real headValue: NaN
  property var linePoints: []
  property var areaPoints: []

  clip: true

  function yOf(v) {
    if (v === null || v === undefined || isNaN(v)) return height
    var c = Math.max(lo, Math.min(hi, v))
    return height - 1 - (c - lo) / Math.max(0.001, hi - lo) * (height - 2)
  }

  function push() {
    if (!live) return
    var vals = values || []
    var n = vals.length
    if (n < 2 || width <= 0) {
      linePoints = []
      areaPoints = []
      return
    }
    var fixed = vals.slice(Math.max(0, n - 2 - window), n - 1)
    var line = []
    var last = NaN
    for (var i = 0; i < fixed.length; i++) {
      var v = fixed[i]
      if (v === null || v === undefined || isNaN(v)) continue
      line.push(Qt.point(i * dx, yOf(v)))
      last = v
    }
    lastIndex = fixed.length - 1
    lastValue = last
    linePoints = line
    areaPoints = line.length ? [Qt.point(line[0].x, height)].concat(line).concat([Qt.point(line[line.length - 1].x, height)]) : []
    var newest = vals[n - 1]
    var target = (newest === null || newest === undefined) ? last : newest
    phaseAnim.stop()
    if (animate && !isNaN(headValue)) {
      phase = 0
      headValue = target
      phaseAnim.restart()
    } else {
      // First frame, or no motion: draw the newest value where it is, with nothing to glide from.
      settling = true
      headValue = target
      settling = false
      phase = 1
    }
  }

  property bool settling: false

  onLiveChanged: if (live) push()
  onWidthChanged: push()

  NumberAnimation {
    id: phaseAnim
    target: root
    property: "phase"
    from: 0
    to: 1
    duration: root.tickMs
    easing.type: Easing.Linear
  }
  Behavior on headValue { enabled: root.animate && !root.settling; NumberAnimation { duration: root.tickMs; easing.type: Easing.Linear } }

  Item {
    id: slide
    height: root.height
    width: Math.max(1, (root.lastIndex + 1) * root.dx)
    x: root.width - root.phase * root.dx - root.lastIndex * root.dx

    Shape {
      anchors.fill: parent
      preferredRendererType: Shape.CurveRenderer
      ShapePath {
        strokeWidth: 0
        strokeColor: "transparent"
        fillColor: Util.alpha(root.foreground, 0.08)
        PathPolyline { path: root.areaPoints }
      }
      ShapePath {
        strokeWidth: 1.25
        strokeColor: Util.alpha(root.foreground, 0.85)
        fillColor: "transparent"
        joinStyle: ShapePath.RoundJoin
        capStyle: ShapePath.RoundCap
        PathPolyline { path: root.linePoints }
      }
    }
  }

  Shape {
    id: head
    anchors.fill: parent
    preferredRendererType: Shape.CurveRenderer
    visible: root.linePoints.length > 0
    readonly property real xk: root.width - root.phase * root.dx
    readonly property real yk: root.yOf(root.lastValue)
    readonly property real yh: root.yOf(root.headValue)
    ShapePath {
      strokeWidth: 0
      strokeColor: "transparent"
      fillColor: Util.alpha(root.foreground, 0.08)
      PathPolyline { path: [Qt.point(head.xk, root.height), Qt.point(head.xk, head.yk), Qt.point(root.width, head.yh), Qt.point(root.width, root.height)] }
    }
    ShapePath {
      strokeWidth: 1.25
      strokeColor: Util.alpha(root.foreground, 0.85)
      fillColor: "transparent"
      capStyle: ShapePath.RoundCap
      PathPolyline { path: [Qt.point(head.xk, head.yk), Qt.point(root.width, head.yh)] }
    }
  }

  Rectangle {
    visible: head.visible
    width: Style.space(3)
    height: width
    radius: width / 2
    color: root.foreground
    x: root.width - width
    y: head.yh - height / 2
  }
}
