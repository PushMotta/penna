import QtQuick
import Quickshell
import Quickshell.Wayland
import qs.Commons
import "penna" as Penna
import "penna/components"
import "penna/Model.js" as Model

// Frame-timing harness for DESIGN.md §8.6, run by run.sh against `pennad --mock`. On the second screen, without taking
// the keyboard: the popup's live rows for 5 s, then the studio with 30 s of live data, then a scripted 10 s point drag
// through the editor's own edit path (local handles, drafts at 10 Hz to the daemon). The studio then closes, which
// discards the draft. Frame intervals come from FrameAnimation; run.sh adds the render thread's own timings.
ShellRoot {
  id: root

  readonly property var screenB: Quickshell.screens.length > 1 ? Quickshell.screens[1] : Quickshell.screens[0]
  readonly property real refreshHz: Number(Quickshell.env("PENNA_REFRESH_HZ")) || 60
  property string phase: "waiting"
  property real phaseStart: 0
  property real started: 0
  property var intervals: ({ popup: [], studio: [], drag: [] })
  property var tempKeys: []
  property var controlKeys: []
  property var openLatency: ({})

  Penna.Service { id: service }

  Penna.Studio {
    id: studio
    service: service
    screenOverride: root.screenB
    grabKeyboard: false
  }

  function enter(next) {
    phase = next
    phaseStart = Date.now()
  }

  function stats(list) {
    var budget = 1000 / refreshHz
    var settled = list.filter(function(x) { return x.at > 500 }).map(function(x) { return x.ms })
    var sorted = settled.slice().sort(function(a, b) { return a - b })
    function pct(p) { return sorted.length ? sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))] : 0 }
    return {
      frames: list.length,
      p50: pct(0.5), p95: pct(0.95), p99: pct(0.99), max: sorted.length ? sorted[sorted.length - 1] : 0,
      dropped: settled.filter(function(ms) { return ms > budget * 1.5 }).length,
      // For each slow frame, how long after the last tick it came: clustered near 0 means the once-a-second work.
      slowAfterTick: list.filter(function(x) { return x.at > 500 && x.ms > budget * 1.5 }).map(function(x) { return Math.round(x.tick) }),
      first: list.length ? Math.max.apply(null, list.filter(function(x) { return x.at <= 500 }).map(function(x) { return x.ms }).concat([0])) : 0
    }
  }

  function finish(error) {
    var out = { refresh_hz: refreshHz, error: error || "" }
    if (!error) {
      out.popup = stats(intervals.popup)
      out.studio = stats(intervals.studio)
      out.drag = stats(intervals.drag)
    }
    console.log("PENNA_RESULT " + JSON.stringify(out))
    quit.start()
  }

  Timer { id: quit; interval: 800; onTriggered: Qt.quit() }

  Timer {
    interval: 100
    repeat: true
    running: true
    onTriggered: {
      var now = Date.now()
      if (root.phase === "waiting") {
        if (root.started === 0) root.started = now
        if (service.connected && service.tick) {
          root.enter("popup")
        } else if (now - root.started > 10000) {
          running = false
          root.finish("pennad --mock is not answering on " + service.socketPath)
        }
        return
      }
      var t = (now - root.phaseStart) / 1000
      if (root.phase === "popup" && t >= 5) {
        studio.open("{}")
        root.enter("studio")
      } else if (root.phase === "studio" && t >= 30) {
        root.enter("drag")
      } else if (root.phase === "drag" && t >= 10) {
        var ed = studio.editorItem
        ed.dragging = -1
        studio.close()
        root.enter("done")
        running = false
        root.finish("")
      }
    }
  }

  PanelWindow {
    id: popupWin
    screen: root.screenB
    anchors { top: true; right: true }
    margins { top: Style.space(34); right: Style.space(12) }
    implicitWidth: Style.space(412)
    implicitHeight: popupColumn.implicitHeight + Style.space(28)
    color: Color.popups.background
    exclusionMode: ExclusionMode.Ignore
    WlrLayershell.namespace: "penna-frame-timing"
    WlrLayershell.layer: WlrLayer.Overlay
    WlrLayershell.keyboardFocus: WlrKeyboardFocus.None

    Column {
      id: popupColumn
      x: Style.space(14)
      y: Style.space(14)
      width: parent.width - Style.space(28)
      spacing: Style.spacing.md

      Text {
        text: "Penna frame timing · " + root.phase
        color: Color.popups.text
        font.family: Style.font.family
        font.pixelSize: Style.font.title
        font.bold: true
      }

      Repeater {
        id: rows
        model: root.tempKeys
        TempRow {
          required property var modelData
          readonly property string key: modelData
          readonly property var row: Model.byKey(Model.tempRows(service.tick), modelData) || ({})
          width: popupColumn.width
          name: row.name || ""
          detail: row.detail || ""
          celsius: row.celsius === null || row.celsius === undefined ? NaN : Number(row.celsius)
          lo: row.lo || 20
          hi: row.hi || 100
          foreground: Color.popups.text
          tickMs: Math.round(service.tickSeconds * 1000)
          live: true
          reducedMotion: false
        }
      }

      Repeater {
        model: root.controlKeys
        FanRow {
          required property var modelData
          width: popupColumn.width
          control: Model.byKey(Model.controls(service.tick), modelData)
          tick: service.tick
          foreground: Color.popups.text
          tickMs: Math.round(service.tickSeconds * 1000)
          live: true
          reducedMotion: false
        }
      }
    }

    Connections {
      target: service
      function onTicked() {
        var t = Model.keysOf(Model.tempRows(service.tick))
        if (!Model.sameKeys(t, root.tempKeys)) root.tempKeys = t
        var c = Model.keysOf(Model.controls(service.tick))
        if (!Model.sameKeys(c, root.controlKeys)) root.controlKeys = c
        for (var i = 0; i < rows.count; i++) {
          var r = rows.itemAt(i)
          if (!r) continue
          r.values = service.history[r.key] || []
          r.push()
        }
      }
    }

    // Every frame: record the interval, and during the drag move the second handle along a figure of eight, the way a
    // hand would, through the editor's own edit path.
    FrameAnimation {
      running: true
      onTriggered: {
        if (frameTime <= 0 || !(root.phase in root.intervals)) return
        var sinceTick = service.lastTickAt > 0 ? Date.now() - service.lastTickAt * 1000 : -1
        root.intervals[root.phase].push({ ms: frameTime * 1000, at: Date.now() - root.phaseStart, tick: sinceTick })
        if (root.phase !== "drag") return
        var ed = studio.editorItem
        if (!ed || ed.kind !== "graph" || ed.handles.length < 2) return
        var s = (Date.now() - root.phaseStart) / 1000
        ed.dragging = 1
        ed.commit(Model.moveHandle("graph", ed.handles, 1, 66 + 8 * Math.sin(s * 2.1), 42 + 14 * Math.sin(s * 4.2)), false)
      }
    }
  }
}
