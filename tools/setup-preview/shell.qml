import QtQuick
import Quickshell
import qs.Commons
import "penna" as Penna
import "penna/components"
import "penna/Model.js" as Model

// Setup's view, rendered offscreen against `pennad --mock x570-desk` (run.sh): a frame of the start screen, then one per step
// as a real run goes through it on this harness's own connection, saved as PNGs. Nothing is shown on a screen.
ShellRoot {
  id: root

  readonly property string out: Quickshell.env("PREVIEW_OUT") || "/tmp"
  readonly property real speed: Number(Quickshell.env("PREVIEW_SPEED")) || 1
  // miasma's tokens: the harness has no theme of its own.
  readonly property color bg: "#222222"
  readonly property color fg: "#c2c2b0"
  readonly property color accent: "#78824b"
  property var shots: ({})
  property bool started: false

  Penna.Service { id: service }
  StudioClient { id: client; path: service.socketPath; active: service.connected }

  function shoot(name) {
    if (root.shots[name]) return
    var s = root.shots
    s[name] = true
    root.shots = s
    view.grabToImage(function(r) {
      console.log("PREVIEW " + name + " " + r.saveToFile(root.out + "/" + name + ".png"))
      if (name === "review") Qt.quit()
    })
  }

  FloatingWindow {
    implicitWidth: 1120
    implicitHeight: 860
    color: root.bg

    Rectangle {
      id: view
      anchors.fill: parent
      color: root.bg
      SetupView {
        id: setupView
        anchors.fill: parent
        anchors.margins: 24
        setup: service.tick ? service.tick.setup : null
        tick: service.tick
        starting: !root.started
        canWrite: client.canWrite
        foreground: root.fg
        accent: root.accent
        urgent: "#685742"
        tickMs: Math.round(service.tickSeconds * 1000)
        live: false
        reducedMotion: true
      }
    }
  }

  Timer {
    interval: 250
    repeat: true
    running: true
    property real since: 0
    property string last: ""
    onTriggered: {
      if (!client.ready || !service.tick) return
      if (!root.started) {
        root.shoot("start")
        root.started = true
        client.setupStart(function(r) { console.log("PREVIEW setup.start " + JSON.stringify(r)) })
        return
      }
      var s = service.tick.setup
      if (!s) return
      if (s.step !== last) { last = s.step; since = Date.now() }
      // Each step a little way in, in simulated seconds, so its panel has something to show.
      var wait = { "idle": 10, "write-test": 2, "pairing": 40, "calibration": 55, "review": 1 }[s.step] || 1
      if ((Date.now() - since) / 1000 * root.speed >= wait) {
        console.log("PREVIEW rows " + setupView.rows.length + " model " + Model.setupRows(s, service.tick).length + " guards " + (s.guards ? s.guards.length : -1))
        root.shoot(s.step)
      }
    }
  }
}
