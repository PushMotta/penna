import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Hyprland
import "Model.js" as Model

// Headless singleton behind every Penna surface (DESIGN.md §8.4): one socket to pennad, the latest tick, five minutes of
// history, and the requests the surfaces send. Surfaces never talk to the daemon themselves. The last tick stays after
// the daemon goes away, so the popup can show the last known values with their age instead of zeros.
Item {
  id: root

  property var shell: null
  property var manifest: null

  readonly property var candidates: Model.socketCandidates(Quickshell.env("PENNA_SOCKET"), Quickshell.env("XDG_RUNTIME_DIR"))
  property int candidateIndex: 0
  readonly property string socketPath: candidates.length ? candidates[candidateIndex % candidates.length] : ""

  property var hello: null
  readonly property bool connected: hello !== null
  readonly property bool canWrite: hello !== null && hello.can_write === true
  // Remembered past a disconnect, so "start Penna" can name the command that was in use.
  property bool lastMock: false
  readonly property real tickSeconds: hello && Number(hello.tick) > 0 ? Number(hello.tick) : 1.0

  property var tick: null
  property real lastTickAt: 0
  // Sensor key → recent values, and "duty:" / "rpm:" + control key → recent values. Oldest first, at most 300.
  property var history: ({})
  readonly property int historyLength: 300
  property string lastError: ""

  // Reduced motion follows Hyprland's own animations switch, as Candela does (§8.6).
  property bool reducedMotion: false

  property var callbacks: ({})
  property int nextId: 1
  property int attempts: 0

  signal ticked()
  signal historyReplaced()
  signal daemonEvent(var event)

  function request(op, args, callback) {
    if (!connected) {
      if (callback) callback({ ok: false, error: "Penna isn't running." })
      return
    }
    var id = nextId++
    var msg = { id: id, op: op }
    for (var k in args) msg[k] = args[k]
    if (callback) callbacks[id] = callback
    sock.write(JSON.stringify(msg) + "\n")
    sock.flush()
  }

  function setProfile(name, callback) { request("profile.set", { name: name }, callback) }
  function setFullSpeed(on, callback) { request("full_speed", { on: on }, callback) }
  function retry(key, callback) { request("retry", { control: key }, callback) }

  function append(key, value) {
    var list = history[key]
    if (!list) {
      list = []
      history[key] = list
    }
    list.push(value === undefined ? null : value)
    if (list.length > historyLength) list.splice(0, list.length - historyLength)
  }

  function ingestTick(msg) {
    // After a write the daemon re-sends the current tick with the same seq (§7.3, as built): the state is news, the
    // second isn't. It updates what surfaces show but adds no history and moves no sparkline.
    if (tick && msg.seq === tick.seq) {
      tick = msg
      return
    }
    var sensors = Model.sensors(msg)
    for (var i = 0; i < sensors.length; i++) append(sensors[i].key, sensors[i].celsius)
    var controls = Model.controls(msg)
    for (var j = 0; j < controls.length; j++) {
      append("duty:" + controls[j].key, controls[j].duty)
      append("rpm:" + controls[j].key, controls[j].rpm)
    }
    tick = msg
    lastTickAt = Date.now() / 1000
    ticked()
  }

  function replaceHistory(msg) {
    var next = {}
    var s = msg.sensors || {}
    for (var key in s) next[key] = (s[key] || []).slice(-historyLength)
    var c = msg.controls || {}
    for (var ckey in c) {
      next["duty:" + ckey] = ((c[ckey] || {}).duty || []).slice(-historyLength)
      next["rpm:" + ckey] = ((c[ckey] || {}).rpm || []).slice(-historyLength)
    }
    history = next
    historyReplaced()
  }

  function handleLine(line) {
    if (!line) return
    var msg
    try {
      msg = JSON.parse(line)
    } catch (e) {
      console.warn("penna: pennad sent a line that is not JSON")
      return
    }
    if (msg.ev === "hello") {
      hello = msg
      lastMock = msg.mock === true
      attempts = 0
      request("subscribe", {})
    } else if (msg.ev === "tick") {
      ingestTick(msg)
    } else if (msg.ev === "history") {
      replaceHistory(msg)
    } else if (msg.ev === "event") {
      daemonEvent(msg)
    } else if (msg.id !== undefined) {
      var cb = callbacks[msg.id]
      if (cb) {
        delete callbacks[msg.id]
        cb(msg)
      }
      if (msg.ok === false) lastError = String(msg.error || "pennad refused the request.")
    }
  }

  function dropConnection() {
    hello = null
    callbacks = ({})
    if (!reconnect.running) reconnect.restart()
  }

  Socket {
    id: sock
    path: root.socketPath
    parser: SplitParser { onRead: function(data) { root.handleLine(data) } }
    onConnectionStateChanged: if (!connected) root.dropConnection()
    onError: function(error) { root.dropConnection() }
  }

  // Walk the candidate sockets quickly, then back off to every 5 s while nothing answers.
  Timer {
    id: reconnect
    interval: root.attempts < root.candidates.length ? 250 : Math.min(5000, 500 * Math.pow(2, Math.floor(root.attempts / root.candidates.length)))
    onTriggered: {
      if (sock.connected) return
      root.attempts++
      root.candidateIndex = (root.candidateIndex + 1) % Math.max(1, root.candidates.length)
      sock.connected = true
    }
  }

  Process {
    id: animations
    command: ["hyprctl", "getoption", "animations:enabled", "-j"]
    stdout: StdioCollector {
      onStreamFinished: {
        try {
          var o = JSON.parse(text)
          var on = o.int !== undefined ? o.int !== 0 : o.bool !== false
          root.reducedMotion = !on
        } catch (e) {
          root.reducedMotion = false
        }
      }
    }
  }

  Connections {
    target: Hyprland
    function onRawEvent(event) { if (event.name === "configreloaded") animations.running = true }
  }

  Component.onCompleted: {
    animations.running = true
    sock.connected = true
  }
}
