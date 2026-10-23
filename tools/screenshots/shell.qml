//@ pragma OmarchyLoadPatch
import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import qs.Commons
import "penna" as Penna

// README and marketplace screenshots (run.sh): the real popup and studio, against `pennad --mock` on $PENNA_SOCKET, on the
// screen $SHOT_SCREEN for a few seconds each, captured with grim into $SHOT_OUT. The pragma above is the Omarchy shell's own:
// without it, `Color` in the plugin's files is QtQuick's, not the shell's palette. A strip stands in for the bar, with the
// popup's glyph at its right end. The popup takes no keyboard focus here (run.sh's copy of the shell's Ui), and neither does
// the studio, so nothing the user is typing elsewhere is taken.
ShellRoot {
  id: root

  readonly property string out: Quickshell.env("SHOT_OUT") || "/tmp"
  readonly property string screenName: Quickshell.env("SHOT_SCREEN") || ""
  readonly property var screen: {
    var all = Quickshell.screens
    for (var i = 0; i < all.length; i++) if (all[i].name === screenName) return all[i]
    return all[0]
  }
  property string phase: "waiting"
  property real started: Date.now()
  // The shell's palette reads the current theme's files once it is first used; the popup and the studio wait for it.
  property bool themeReady: false

  Penna.Service { id: daemon }

  // What Popup.qml and the shell's Ui read from the bar.
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
    }
    function requestPopout(key) { activePopout = key }
    function releasePopout(key) { if (activePopout === key) activePopout = null }
    function switchPanelFrom(panel, direction) { return false }
    function showTooltip(item, text) {}
    function hideTooltip(item) {}
    function targetBelongsToWindow(target, window) { return false }
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

  Process {
    id: grim
    property string name: ""
    command: ["grim", "-o", root.screen.name, root.out + "/" + name + "-full.png"]
    onExited: function(code) { console.log("SHOT " + name + " " + code) }
  }

  function shoot(name) {
    grim.name = name
    grim.running = true
  }

  Timer {
    interval: 250
    repeat: true
    running: true
    onTriggered: {
      var t = (Date.now() - root.started) / 1000
      if (!root.themeReady) {
        root.themeReady = Color.popups !== undefined && Color.popups !== null && t > 1.5
        return
      }
      if (root.phase === "waiting") {
        if (daemon.connected && daemon.tick) { root.phase = "popup"; root.started = Date.now(); popup.open() }
        else if (t > 15) { console.log("SHOT error pennad --mock is not answering"); Qt.quit() }
      } else if (root.phase === "popup" && t >= 3) {
        console.log("SHOT popup opened=" + popup.opened + " connected=" + daemon.connected)
        root.shoot("popup")
        root.phase = "popup-shot"
      } else if (root.phase === "popup-shot" && t >= 4) {
        popup.close()
        studio.open("{}")
        root.phase = "studio"
        root.started = Date.now()
      } else if (root.phase === "studio" && t >= 2) {
        console.log("SHOT studio opened=" + studio.opened + " connected=" + studio.connected)
        root.shoot("studio-2s")
        root.phase = "studio-a"
      } else if (root.phase === "studio-a" && t >= 5) {
        root.shoot("studio-5s")
        root.phase = "studio-b"
      } else if (root.phase === "studio-b" && t >= 9) {
        root.shoot("studio")
        root.phase = "studio-shot"
      } else if (root.phase === "studio-shot" && t >= 10) {
        studio.close()
        root.phase = "done"
        console.log("SHOT done")
        quitLater.start()
      }
    }
  }

  Timer { id: quitLater; interval: 600; onTriggered: Qt.quit() }
}
