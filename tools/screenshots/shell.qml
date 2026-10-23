//@ pragma OmarchyLoadPatch
import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import qs.Commons
import "penna" as Penna
import "penna/Model.js" as Model
import "themes.js" as Themes

// README screenshots and GIFs (run.sh): the real popup and studio against `pennad --mock` on $PENNA_SOCKET, on the screen
// $SHOT_SCREEN, following a script. A backdrop covers that screen, so nothing else on it is captured, and a strip stands in
// for the bar with the popup's glyph at its right end. Nothing takes the keyboard: the popup's panel is run.sh's copy of the
// shell's Ui, which never asks for focus, and the studio opens without its grab.
//
// $SHOT_MODE `tour` runs a fresh install of the x570 desk through setup, the popup and the studio. `laptop` shows the popup and
// the studio on a machine already set up; `themes`, the studio in the current theme and in each other theme in themes.js. Stills are grim PNGs in
// $SHOT_OUT; each recorded stretch is logged as `MARK <name> start|end <ms since the epoch>` for run.sh to cut from its
// screen recording.
//
// The pragma above is the Omarchy shell's own: without it, `Color` in the plugin's files is QtQuick's, not the shell's palette.
ShellRoot {
  id: root

  readonly property string out: Quickshell.env("SHOT_OUT") || "/tmp"
  readonly property string mode: Quickshell.env("SHOT_MODE") || "tour"
  // Each theme's colors.toml and shell.toml, and the current theme's, written into themes.js by run.sh.
  readonly property var themes: Object.keys(Themes.themes).filter(function(n) { return n !== "current" })
  readonly property string screenName: Quickshell.env("SHOT_SCREEN") || ""
  readonly property var screen: {
    var all = Quickshell.screens
    for (var i = 0; i < all.length; i++) if (all[i].name === screenName) return all[i]
    return all[0]
  }
  readonly property var tick: daemon.tick
  readonly property var setup: tick && tick.setup ? tick.setup : null
  readonly property string cpuFan: "hwmon/it8688_4008090b/it87.2624/pwm1"

  Penna.Service { id: daemon }

  // What Popup.qml and the shell's Ui read from the bar. `summon` is the shell opening a plugin's overlay: the studio.
  QtObject {
    id: fakeBar
    property string position: "top"
    property bool vertical: false
    property int barSize: Style.bar.sizeHorizontal
    property color foreground: Color.foreground
    property color barForeground: Color.foreground
    property color urgent: Color.urgent
    property string fontFamily: Style.font.family
    property bool foregroundAnimationEnabled: true
    property var activePopout: null
    property var clickTargets: []
    property QtObject shell: QtObject {
      function serviceFor(id) { return daemon }
      function summon(id, payload) { studio.open(payload || "{}") }
    }
    function requestPopout(key) { activePopout = key }
    function releasePopout(key) { if (activePopout === key) activePopout = null }
    function switchPanelFrom(panel, direction) { return false }
    function showTooltip(item, text) {}
    function hideTooltip(item) {}
    function targetBelongsToWindow(target, window) { return false }
  }

  // A plain backdrop under everything Penna shows, above the screen's own windows.
  PanelWindow {
    screen: root.screen
    anchors { top: true; bottom: true; left: true; right: true }
    color: Color.background
    exclusionMode: ExclusionMode.Ignore
    WlrLayershell.namespace: "penna-screenshots-backdrop"
    WlrLayershell.layer: WlrLayer.Top
    WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
  }

  PanelWindow {
    id: strip
    screen: root.screen
    anchors { top: true; left: true; right: true }
    implicitHeight: Style.bar.sizeHorizontal
    color: Color.background
    exclusionMode: ExclusionMode.Ignore
    WlrLayershell.namespace: "penna-screenshots"
    WlrLayershell.layer: WlrLayer.Overlay
    WlrLayershell.keyboardFocus: WlrKeyboardFocus.None

    Penna.Popup {
      id: popup
      bar: fakeBar
      anchors.right: parent.right
      anchors.rightMargin: Style.space(16)
      anchors.verticalCenter: parent.verticalCenter
      width: Style.bar.iconSlot
      height: Style.bar.sizeHorizontal
    }
  }

  Penna.Studio {
    id: studio
    service: daemon
    screenOverride: root.screen
    grabKeyboard: false
  }

  // ---------------------------------------------------------- captures
  property var shotQueue: []
  Process {
    id: grim
    property string name: ""
    command: ["grim", "-o", root.screen.name, root.out + "/" + name + ".png"]
    onExited: function(code) {
      console.log("SHOT " + name + " " + code)
      if (root.shotQueue.length) Qt.callLater(root.nextShot)
    }
  }
  function nextShot() {
    if (grim.running || !shotQueue.length) return
    var q = shotQueue
    grim.name = q.shift()
    shotQueue = q
    grim.running = true
  }
  function shoot(name) {
    var q = shotQueue
    q.push(name)
    shotQueue = q
    nextShot()
  }
  function mark(name, what) { console.log("MARK " + name + " " + what + " " + Date.now()) }

  // ---------------------------------------------------------- themes
  // As the shell's own applyTheme does.
  function loadTheme(name) {
    var t = Themes.themes[name]
    Color.loadColors(t.colors)
    Color.loadShell(t.shell)
    Style.scheduleRefresh()
  }

  // ---------------------------------------------------------- a curve point dragged by hand, smoothly
  property real dragFrom: 0
  property var dragStart: null
  property int dragIndex: 2
  FrameAnimation {
    id: dragger
    running: false
    onTriggered: {
      var ed = studio.editorItem
      if (!ed || ed.kind !== "graph" || !root.dragStart) { running = false; return }
      var k = Math.min(1, (Date.now() - root.dragFrom) / 2400)
      var e = 0.5 - 0.5 * Math.cos(Math.PI * k)
      var p = root.dragStart
      ed.dragging = root.dragIndex
      ed.commit(Model.moveHandle("graph", ed.handles, root.dragIndex, p[0] + 4 * e, p[1] - 12 * e), k >= 1)
      if (k >= 1) {
        ed.dragging = -1
        running = false
      }
    }
  }
  function startDrag() {
    var ed = studio.editorItem
    if (!ed || ed.kind !== "graph" || ed.handles.length <= dragIndex) return
    dragStart = [ed.handles[dragIndex][0], ed.handles[dragIndex][1]]
    dragFrom = Date.now()
    dragger.running = true
  }

  // ---------------------------------------------------------- the script
  // Each step is an action, a wait in seconds, or a condition to wait for, with its own limit.
  function step(name) { return root.setup && root.setup.step === name }
  readonly property var tour: [
    { until: function() { return root.ready() }, limit: 20 },
    { sleep: 1 },
    { act: function() { popup.open() } }, { sleep: 2.5 }, { act: function() { root.shoot("popup-defaults") } }, { sleep: 0.8 },
    { act: function() { root.mark("setup", "start"); popup.openSetup() } }, { sleep: 2.5 },
    { act: function() { root.shoot("setup-start") } }, { sleep: 1 },
    { act: function() { studio.startSetup() } },
    { until: function() { return root.step("pairing") }, limit: 60 }, { sleep: 1.5 },
    { act: function() { root.shoot("setup-pairing") } },
    { until: function() { return root.step("calibration") }, limit: 60 }, { sleep: 5 },
    { act: function() { root.shoot("setup-calibration") } },
    { until: function() { return root.step("load-check") || root.step("review") }, limit: 90 }, { sleep: 3 },
    { act: function() { root.shoot("setup-load") } },
    { until: function() { return root.step("review") }, limit: 90 }, { sleep: 2 },
    { act: function() { root.shoot("setup-review"); root.mark("setup", "end") } }, { sleep: 1 },
    { act: function() { studio.confirmRoles() } }, { sleep: 1 },
    { until: function() { return studio.curvesWait === "" && root.step("review") }, limit: 90 }, { sleep: 2.5 },
    { act: function() { root.shoot("curves-draft") } }, { sleep: 1 },
    { act: function() { studio.save() } }, { sleep: 2 }, { act: function() { studio.close() } },
    // Balanced, long enough for every sparkline to fill.
    { sleep: 16 },
    { act: function() { root.mark("popup", "start"); popup.open() } }, { sleep: 2.5 },
    { act: function() { root.shoot("popup") } }, { sleep: 1 },
    { act: function() { popup.chooseProfile("quiet") } }, { sleep: 3 },
    { act: function() { popup.chooseProfile("render") } }, { sleep: 3 },
    { act: function() { popup.chooseProfile("balanced") } }, { sleep: 2.5 },
    { act: function() { popup.toggleFullSpeed() } }, { sleep: 3 }, { act: function() { root.shoot("popup-full-speed") } },
    { sleep: 0.5 }, { act: function() { popup.toggleFullSpeed() } }, { sleep: 2.5 },
    { act: function() { root.mark("popup", "end"); root.mark("studio", "start"); popup.openStudio() } }, { sleep: 2 },
    { act: function() { studio.selectControl(root.cpuFan) } }, { sleep: 2 },
    { act: function() { root.shoot("studio") } }, { sleep: 0.5 },
    { act: function() { root.startDrag() } }, { sleep: 3.5 },
    { act: function() { root.shoot("studio-edited") } }, { sleep: 0.5 },
    { act: function() { studio.save() } }, { sleep: 2.5 },
    { act: function() { root.mark("studio", "end") } },
    { act: function() { studio.close() } }, { sleep: 1 }
  ]
  readonly property var themed: [
    { until: function() { return root.ready() }, limit: 20 }, { sleep: 2 },
    { act: function() { studio.open("{}") } }, { sleep: 1.5 },
    { act: function() { studio.selectControl(root.cpuFan) } }, { sleep: 2 },
    { act: function() { root.shoot("theme-current") } }, { sleep: 0.5 },
    { themes: true },
    { act: function() { root.loadTheme("current") } }, { sleep: 1 },
    { act: function() { studio.close() } }, { sleep: 1 }
  ]
  readonly property var laptop: [
    { until: function() { return root.ready() }, limit: 20 }, { sleep: 2 },
    { act: function() { popup.open() } }, { sleep: 2.5 }, { act: function() { root.shoot("laptop-popup") } }, { sleep: 0.5 },
    { act: function() { popup.openStudio() } }, { sleep: 3 },
    { act: function() { root.shoot("laptop-studio") } }, { sleep: 1 },
    { act: function() { studio.close() } }, { sleep: 1 }
  ]

  property bool themeReady: false
  function ready() { return themeReady && daemon.connected && daemon.tick !== null }

  property var steps: []
  property int at: 0
  property real since: Date.now()
  property real started: Date.now()

  Component.onCompleted: {
    var s = (mode === "laptop" ? laptop : mode === "themes" ? themed : tour).slice()
    var i = s.findIndex(function(x) { return x.themes })
    if (i >= 0) {
      var t = []
      themes.forEach(function(name) {
        t.push({ act: function() { root.loadTheme(name) } }, { sleep: 1.4 },
               { act: function() { root.shoot("theme-" + name) } }, { sleep: 0.6 })
      })
      s.splice.apply(s, [i, 1].concat(t))
    }
    steps = s
  }

  Timer {
    interval: 100
    repeat: true
    running: true
    onTriggered: {
      if (!root.themeReady) {
        root.themeReady = Color.popups !== undefined && Color.popups !== null && (Date.now() - root.started) > 1500
        return
      }
      if (root.at >= root.steps.length) {
        if (!grim.running && !root.shotQueue.length) {
          running = false
          console.log("SHOT done")
          quitLater.start()
        }
        return
      }
      var s = root.steps[root.at]
      var t = (Date.now() - root.since) / 1000
      var next = false
      if (s.act) { s.act(); next = true }
      else if (s.sleep !== undefined) next = t >= s.sleep
      else if (s.until) {
        if (s.until()) next = true
        else if (t > s.limit) {
          console.log("SHOT error step " + root.at + " waited " + s.limit + " s" + (root.setup ? " in " + root.setup.step : ""))
          root.at = root.steps.length
          return
        }
      }
      if (next) {
        root.at++
        root.since = Date.now()
      }
    }
  }

  Timer { id: quitLater; interval: 600; onTriggered: Qt.quit() }
}
