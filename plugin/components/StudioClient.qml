import QtQuick
import Quickshell.Io

// The studio's own connection to pennad. The draft belongs to this connection (DESIGN.md §7.4): closing it — Esc, a closed
// overlay, a shell crash — makes the daemon return to the saved configuration. Live readings come from the shared service;
// this connection only carries the studio body and the draft.
Item {
  id: root

  property string path: ""
  property bool active: false
  property var body: null
  property bool ready: false
  property bool canWrite: false
  property string error: ""

  property var callbacks: ({})
  property int nextId: 1

  readonly property bool dirty: body !== null && body.draft && body.draft.mine === true && body.draft.dirty === true
  readonly property bool lockedByOther: body !== null && body.draft && body.draft.dirty === true && body.draft.mine !== true

  function request(op, args, callback) {
    if (!ready) {
      if (callback) callback({ ok: false, error: "The studio isn't connected to Penna." })
      return
    }
    var id = nextId++
    var msg = { id: id, op: op }
    for (var k in args) msg[k] = args[k]
    if (callback) callbacks[id] = callback
    sock.write(JSON.stringify(msg) + "\n")
    sock.flush()
  }

  // Every draft op answers with the whole studio body; a refusal keeps the body and says why.
  function draftOp(op, args, callback) {
    request(op, args, function(reply) {
      if (reply.ok && reply.result && reply.result.curves) {
        root.body = reply.result
        root.error = ""
      } else if (!reply.ok) {
        root.error = String(reply.error || "Penna refused the change.")
      }
      if (callback) callback(reply)
    })
  }

  function refresh() { request("studio", {}, function(r) { if (r.ok) root.body = r.result }) }
  function putCurve(curve, callback) { draftOp("draft.put_curve", { curve: curve }, callback) }
  // In `profile`, the one the studio shows, which needn't be the one running (§8.3).
  function assign(control, curve, profile, callback) { draftOp("draft.assign", { control: control, curve: curve, profile: profile }, callback) }
  function discard(callback) { draftOp("draft.discard", {}, callback) }
  function save(callback) {
    request("draft.save", {}, function(reply) {
      if (reply.ok) {
        root.error = ""
        root.refresh()
      } else {
        root.error = String(reply.error || "Penna couldn't save the changes.")
      }
      if (callback) callback(reply)
    })
  }

  // Setup (§5) runs on this connection too: closing the studio stops it and every fan goes back.
  function setupStart(loadCheck, callback) { request("setup.start", { load_check: loadCheck }, callback) }
  function setupStop(callback) { request("setup.stop", {}, callback) }
  function setupRoles(roles, callback) { request("setup.roles", { roles: roles }, callback) }
  function identify(control, callback) { request("identify", { control: control }, callback) }
  function label(key, label, callback) {
    request("label", { key: key, label: label }, function(reply) {
      if (reply.ok) root.refresh()
      if (callback) callback(reply)
    })
  }

  function handleLine(line) {
    if (!line) return
    var msg
    try {
      msg = JSON.parse(line)
    } catch (e) {
      return
    }
    if (msg.ev === "hello") {
      ready = true
      canWrite = msg.can_write === true
      refresh()
    } else if (msg.id !== undefined) {
      var cb = callbacks[msg.id]
      if (cb) {
        delete callbacks[msg.id]
        cb(msg)
      }
    }
  }

  onActiveChanged: {
    if (active) {
      body = null
      error = ""
      sock.connected = true
    } else {
      sock.connected = false
      ready = false
      callbacks = ({})
    }
  }

  Socket {
    id: sock
    path: root.path
    parser: SplitParser { onRead: function(data) { root.handleLine(data) } }
    onConnectionStateChanged: if (!connected) {
      root.ready = false
      root.callbacks = ({})
      if (root.active) retry.restart()
    }
    onError: function(error) { if (root.active) retry.restart() }
  }

  Timer {
    id: retry
    interval: 1000
    onTriggered: if (root.active && !sock.connected) sock.connected = true
  }
}
