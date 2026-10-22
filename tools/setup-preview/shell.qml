import QtQuick
import Quickshell
import qs.Commons
import qs.Ui
import "penna" as Penna
import "penna/components"
import "penna/Model.js" as Model

// Setup's view, rendered offscreen against `pennad --mock x570-desk` (run.sh): a frame of the start screen, then one per step
// as a real run goes through it on this harness's own connection, saved as PNGs. After the review it confirms every suggested
// role and renders the editor on the draft setup proposed (§5.5), as the studio's centre column shows it. Nothing is shown on
// a screen.
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
  property string phase: "setup"      // then "confirming", "curves"
  property var body: client.body
  property string view: Model.viewProfile(body, "")
  property string curveKey: "hwmon/it8688_4008090b/it87.2624/pwm1"
  readonly property var shownProfile: body ? body.profiles.filter(function(p) { return p.id === root.view })[0] : null
  readonly property var curve: body && shownProfile ? body.curves.filter(function(c) { return c.id === shownProfile.assign[root.curveKey] })[0] || null : null
  readonly property var control: body ? body.controls.filter(function(c) { return c.key === root.curveKey })[0] || null : null

  Penna.Service { id: service }
  StudioClient { id: client; path: service.socketPath; active: service.connected }

  function shoot(name, item, then) {
    if (root.shots[name]) return
    var s = root.shots
    s[name] = true
    root.shots = s
    item.grabToImage(function(r) {
      console.log("PREVIEW " + name + " " + r.saveToFile(root.out + "/" + name + ".png"))
      if (then) then()
    })
  }

  Connections {
    target: service
    function onTicked() { setupView.ticked() }
  }

  FloatingWindow {
    implicitWidth: 1120
    implicitHeight: 860
    color: root.bg

    Rectangle {
      id: view
      anchors.fill: parent
      color: root.bg
      visible: root.phase !== "curves"
      SetupView {
        id: setupView
        anchors.fill: parent
        anchors.margins: 24
        setup: service.tick ? service.tick.setup : null
        tick: service.tick
        history: service.history
        starting: !root.started
        canWrite: client.canWrite
        foreground: root.fg
        accent: root.accent
        urgent: "#685742"
        tickMs: Math.round(service.tickSeconds * 1000)
        live: true
        reducedMotion: true
      }
    }

    // The studio's centre column on setup's draft: the profile shown, the Generated chip, the anchors.
    Rectangle {
      id: editorView
      anchors.fill: parent
      color: root.bg
      visible: root.phase === "curves"
      Column {
        anchors.fill: parent
        anchors.margins: 24
        spacing: Style.spacing.lg
        Row {
          spacing: Style.spacing.lg
          ProfilePills {
            options: Model.profileTabs(root.body)
            value: root.view
            foreground: root.fg
          }
          Text { anchors.verticalCenter: parent.verticalCenter; text: root.body ? Model.profileLabel(root.body) + " runs" : ""; color: Qt.darker(root.fg, 1.4); font.pixelSize: Style.font.bodySmall }
          Text { anchors.verticalCenter: parent.verticalCenter; text: Model.draftLine(root.body); color: root.accent; font.pixelSize: Style.font.bodySmall }
        }
        Text { text: root.curve ? root.curve.label : "no curve"; color: root.fg; font.pixelSize: Style.font.title; font.bold: true }
        Text {
          text: (root.curve && Model.isGenerated(root.body, root.curve.id) ? "[Generated]  " : "") + Model.saveLabel(root.body)
          color: root.accent
          font.pixelSize: Style.font.caption
        }
        Rectangle {
          width: parent.width
          height: 420
          color: "transparent"
          border.width: 1
          border.color: Util.alpha(root.fg, 0.12)
          CurveEditor {
            anchors.fill: parent
            anchors.margins: Style.spacing.lg
            curve: root.curve
            control: root.control
            marks: Model.anchorMarks(root.body, root.curve)
            running: false
            reducedMotion: true
            foreground: root.fg
            accent: root.accent
          }
        }
        Text { text: Model.limitsLine(root.control); color: Qt.darker(root.fg, 1.4); font.pixelSize: Style.font.caption }
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
        // The start screen is grabbed before setup starts, or the grab would catch the first step.
        root.shoot("start", view, function() {
          root.started = true
          client.setupStart(true, function(r) { console.log("PREVIEW setup.start " + JSON.stringify(r)) })
        })
        return
      }
      if (root.phase === "curves") {
        if (!root.body || !root.curve) return
        // The CPU fan, then the GPU's, in the profile shown: Balanced, where saving goes.
        if (root.curveKey.indexOf("nvml/") < 0) {
          root.shoot("curves-cpu", editorView, function() {
            root.curveKey = root.body.controls.filter(function(c) { return c.key.indexOf("nvml/") === 0 })[0].key
          })
        } else {
          root.shoot("curves-gpu", editorView, function() { Qt.quit() })
        }
        return
      }
      var s = service.tick.setup
      if (!s) return
      if (s.step !== last) { last = s.step; since = Date.now() }
      // Each step a little way in, in simulated seconds, so its panel has something to show.
      var wait = { "idle": 10, "write-test": 2, "pairing": 40, "calibration": 55, "load-check": 60, "review": 1 }[s.step] || 1
      if ((Date.now() - since) / 1000 * root.speed >= wait && root.phase === "setup") {
        console.log("PREVIEW rows " + setupView.rows.length + " model " + Model.setupRows(s, service.tick).length + " guards " + (s.guards ? s.guards.length : -1))
        root.shoot(s.step, view, s.step !== "review" ? null : function() {
          root.phase = "confirming"
          client.setupRoles(setupView.chosenRoles(), function(r) {
            console.log("PREVIEW setup.roles " + JSON.stringify(r))
            client.refresh()
            root.phase = "curves"
          })
        })
      }
    }
  }
}
