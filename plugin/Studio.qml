import QtQuick
import Quickshell
import Quickshell.Wayland
import Quickshell.Hyprland
import qs.Commons
import qs.Ui
import "Model.js" as Model
import "components"

// The studio (DESIGN.md §8.3): controls on the left, the curve editor in the middle, sensors on the right, the action bar
// along the bottom. Live readings come from the shared service; edits go through the studio's own connection, so an
// unsaved draft dies with it — Esc, closing, or a shell crash all return the fans to the saved configuration (§7.4).
Item {
  id: root

  property var shell: null
  property var manifest: null
  property var service: null

  property bool opened: false
  property var targetScreen: null
  // For the frame-timing harness (test/frame-timing): a fixed screen, and no keyboard grab.
  property var screenOverride: null
  property bool grabKeyboard: true
  readonly property alias editorItem: editor

  readonly property var tick: service ? service.tick : null
  readonly property bool connected: service ? service.connected : false
  readonly property bool reducedMotion: service ? service.reducedMotion : false
  readonly property int tickMs: service ? Math.round(service.tickSeconds * 1000) : 1000
  readonly property bool live: opened && connected
  readonly property var body: client.body
  readonly property bool canWrite: client.canWrite

  readonly property color background: Color.popups.background
  readonly property color foreground: Color.popups.text
  readonly property color accent: Color.accent
  readonly property color urgent: Color.urgent
  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property color scrim: Color.menu.scrim
  readonly property string fontFamily: Style.font.family
  readonly property var borderSpec: Border.surfaceSpec("popups", "border", Color.popups.border, Math.max(1, Style.space(2)))

  // ---------------------------------------------------------- what is selected
  readonly property var liveControls: Model.controls(tick)
  property string selectedKey: ""
  readonly property var selectedLive: find(liveControls, "key", selectedKey)
  readonly property var selectedInfo: body ? find(body.controls, "key", selectedKey) : null
  readonly property var activeProfile: body ? find(body.profiles, "id", body.profile) : null
  readonly property string selectedCurveId: activeProfile && activeProfile.assign && activeProfile.assign[selectedKey] ? String(activeProfile.assign[selectedKey]) : "firmware"
  readonly property var selectedCurve: body ? find(body.curves, "id", selectedCurveId) : null
  readonly property var selectedCurveLive: tick ? find(tick.curves, "id", selectedCurveId) : null
  readonly property var plausibleSensors: Model.sensors(tick).filter(function(s) { return !s.below_ambient && s.celsius !== null && s.celsius !== undefined })
  readonly property var coldSensors: Model.sensors(tick).filter(function(s) { return s.below_ambient })

  // A drag sends drafts ten times a second and each reply is a whole studio body; lists built from it are replaced only
  // when their content changes, so the dropdown and the chips aren't rebuilt under the hand.
  property var curveOptionList: []
  property var chipList: []
  function syncLists() {
    var o = Model.curveOptions(body)
    if (JSON.stringify(o) !== JSON.stringify(curveOptionList)) curveOptionList = o
    var c = selectedCurve
    var chips = !c ? []
              : c.kind === "graph" ? ["Graph", c.source_label || "", "Response " + c.response + " s", "Hysteresis " + c.hysteresis + " °C"]
              : c.kind === "trigger" ? ["Trigger", c.source_label || "", "On at " + c.trigger.on + " °C, off at " + c.trigger.off + " °C"]
              : c.kind === "mix" ? ["Mix", c.mix.op]
              : ["Flat"]
    if (JSON.stringify(chips) !== JSON.stringify(chipList)) chipList = chips
  }
  onBodyChanged: syncLists()
  onSelectedCurveChanged: syncLists()

  // Repeater keys: replaced only when the set of rows changes, so cards and sparklines survive ticks (§8.6).
  property var controlKeys: []
  property var sensorKeys: []
  function syncKeys() {
    var c = Model.keysOf(liveControls)
    if (!Model.sameKeys(c, controlKeys)) controlKeys = c
    var s = Model.keysOf(plausibleSensors)
    if (!Model.sameKeys(s, sensorKeys)) sensorKeys = s
  }

  function find(list, field, value) {
    if (!Array.isArray(list)) return null
    for (var i = 0; i < list.length; i++) if (list[i][field] === value) return list[i]
    return null
  }

  function labelsOf(keys) {
    return (keys || []).map(function(k) {
      var c = root.find(root.liveControls, "key", k)
      return c ? c.label : k
    })
  }

  // ---------------------------------------------------------- keyboard model
  // Three areas, Tab between them: controls (j/k), the editor ([ ] h/l j/k a x), the action bar (h/l, Enter).
  property string focusArea: "controls"
  property int actionIndex: 3
  property bool closeArmed: false
  property string notice: ""

  readonly property var actions: [
    { id: "pair", label: "Pair…", enabled: false, tip: "Pairing arrives with motherboard fans (M5)" },
    { id: "calibrate", label: "Calibrate…", enabled: false, tip: "Calibration arrives with motherboard fans (M5)" },
    { id: "discard", label: "Discard", enabled: client.dirty, tip: "" },
    { id: "save", label: "Save", enabled: client.dirty, tip: "" }
  ]

  function open(payloadJson) {
    var wanted = Hyprland.focusedMonitor ? String(Hyprland.focusedMonitor.name) : ""
    var chosen = null
    for (var i = 0; i < Quickshell.screens.length; i++) if (Quickshell.screens[i].name === wanted) chosen = Quickshell.screens[i]
    targetScreen = screenOverride || chosen || (Quickshell.screens.length ? Quickshell.screens[0] : null)
    if (!selectedKey || !find(liveControls, "key", selectedKey)) selectedKey = liveControls.length ? String(liveControls[0].key) : ""
    syncKeys()
    focusArea = "controls"
    closeArmed = false
    notice = ""
    client.path = service ? service.socketPath : ""
    client.active = true
    opened = true
    Qt.callLater(function() {
      keyScope.forceActiveFocus()
      root.pushAll()
    })
  }

  function close() {
    opened = false
    closeArmed = false
    pendingCurve = null
    client.active = false
  }

  // Unsaved changes ask once: the first Esc arms, the second discards and closes.
  function requestClose() {
    if (client.dirty && !closeArmed) {
      closeArmed = true
      return
    }
    close()
  }

  function save() {
    if (!client.dirty) return
    flushPending()
    client.save(function(reply) {
      if (reply.ok) {
        root.closeArmed = false
        root.say(reply.result && reply.result.text ? reply.result.text : "Saved.")
      }
    })
  }

  function discard() {
    if (!client.dirty) return
    pendingCurve = null
    client.discard(function(reply) {
      root.closeArmed = false
      editor.syncFromCurve()
      if (reply.ok) root.say("Back to the saved curves.")
    })
  }

  function say(text) {
    notice = String(text || "")
    noticeTimer.restart()
  }

  function selectControl(key) {
    flushPending()
    selectedKey = key
  }

  function moveControl(step) {
    var i = -1
    for (var j = 0; j < liveControls.length; j++) if (liveControls[j].key === selectedKey) i = j
    var next = Math.max(0, Math.min(liveControls.length - 1, i + step))
    if (liveControls[next]) selectControl(String(liveControls[next].key))
  }

  function activateAction(i) {
    var a = actions[i]
    if (!a || !a.enabled) return
    if (a.id === "save") save()
    else if (a.id === "discard") discard()
  }

  function handleKey(event) {
    var k = event.key
    var shift = (event.modifiers & Qt.ShiftModifier) !== 0
    var ctrl = (event.modifiers & Qt.ControlModifier) !== 0
    var step = shift ? 5 : 1
    if (k === Qt.Key_Escape) { requestClose(); return true }
    if (ctrl && k === Qt.Key_S) { save(); return true }
    if (k === Qt.Key_Tab || k === Qt.Key_Backtab) {
      var order = ["controls", "editor", "actions"]
      var i = order.indexOf(focusArea)
      focusArea = order[(i + (k === Qt.Key_Backtab || shift ? 2 : 1)) % 3]
      return true
    }
    closeArmed = false
    if (focusArea === "controls") {
      if (k === Qt.Key_J || k === Qt.Key_Down) { moveControl(1); return true }
      if (k === Qt.Key_K || k === Qt.Key_Up) { moveControl(-1); return true }
      if (k === Qt.Key_Return || k === Qt.Key_Enter || k === Qt.Key_L || k === Qt.Key_Right) { focusArea = "editor"; return true }
      return false
    }
    if (focusArea === "editor") {
      if (!canWrite) return false
      if (k === Qt.Key_BracketLeft) { editor.selectBy(-1); return true }
      if (k === Qt.Key_BracketRight) { editor.selectBy(1); return true }
      if (k === Qt.Key_H || k === Qt.Key_Left) { editor.nudge(-step, 0); return true }
      if (k === Qt.Key_L || k === Qt.Key_Right) { editor.nudge(step, 0); return true }
      if (k === Qt.Key_K || k === Qt.Key_Up) { editor.nudge(0, step); return true }
      if (k === Qt.Key_J || k === Qt.Key_Down) { editor.nudge(0, -step); return true }
      if (k === Qt.Key_A) { editor.addHandle(); return true }
      if (k === Qt.Key_X) { editor.removeHandle(); return true }
      return false
    }
    if (focusArea === "actions") {
      if (k === Qt.Key_H || k === Qt.Key_Left) { actionIndex = Math.max(0, actionIndex - 1); return true }
      if (k === Qt.Key_L || k === Qt.Key_Right) { actionIndex = Math.min(actions.length - 1, actionIndex + 1); return true }
      if (k === Qt.Key_Return || k === Qt.Key_Enter || k === Qt.Key_Space) { activateAction(actionIndex); return true }
    }
    return false
  }

  // ---------------------------------------------------------- drafts, at most 10 a second plus the final value (§8.6)
  property var pendingCurve: null

  function onEdited(curve, final) {
    if (!canWrite) return
    closeArmed = false
    pendingCurve = curve
    if (final) flushPending()
    else if (!sendTimer.running) sendTimer.start()
  }

  function flushPending() {
    sendTimer.stop()
    if (!pendingCurve) return
    var c = pendingCurve
    pendingCurve = null
    client.putCurve(c, function(reply) { if (!reply.ok) editor.syncFromCurve() })
  }

  Timer {
    id: sendTimer
    interval: 100
    onTriggered: root.flushPending()
  }

  Timer {
    id: noticeTimer
    interval: 5000
    onTriggered: root.notice = ""
  }

  // ---------------------------------------------------------- live data
  // Nine five-minute sparklines rebuilt in one frame cost that frame (§8.6, measured with tools/frame-timing). They are
  // rebuilt one per frame instead; each glides from its own moment, so the stagger doesn't show.
  property var pushQueue: []

  function pushAll() {
    var q = []
    for (var i = 0; i < sensorRepeater.count; i++) q.push(i)
    pushQueue = q
    stagger.running = q.length > 0
  }

  FrameAnimation {
    id: stagger
    running: false
    onTriggered: {
      var q = root.pushQueue
      var i = q.length ? q.shift() : -1
      var row = i >= 0 ? sensorRepeater.itemAt(i) : null
      if (row && root.service) {
        row.values = root.service.history[row.key] || []
        row.push()
      }
      if (!q.length) running = false
    }
  }

  Connections {
    target: root.service
    function onTicked() {
      if (!root.opened) return
      root.syncKeys()
      root.pushAll()
    }
    function onHistoryReplaced() { if (root.opened) root.pushAll() }
  }

  StudioClient { id: client }

  readonly property string hints: focusArea === "controls" ? "j/k control · ↵ edit its curve · ⇥ editor · esc close"
                                : focusArea === "editor" ? "[ ] point · h/l °C · j/k % · ⇧ ×5 · a add · x delete · ⇥ actions · ctrl+s save"
                                : "h/l action · ↵ choose · ⇥ controls · esc close"

  // ---------------------------------------------------------- window
  PanelWindow {
    id: window
    visible: root.opened || card.opacity > 0
    screen: root.targetScreen
    anchors { top: true; bottom: true; left: true; right: true }
    color: "transparent"
    exclusionMode: ExclusionMode.Ignore
    WlrLayershell.namespace: "penna-studio"
    WlrLayershell.layer: WlrLayer.Overlay
    WlrLayershell.keyboardFocus: root.opened && root.grabKeyboard ? WlrKeyboardFocus.Exclusive : WlrKeyboardFocus.None

    Rectangle {
      anchors.fill: parent
      color: root.scrim
      opacity: root.opened ? 1 : 0
      Behavior on opacity { enabled: !root.reducedMotion; NumberAnimation { duration: 140; easing.type: Easing.OutCubic } }
      MouseArea { anchors.fill: parent; onClicked: root.requestClose() }
    }

    BorderSurface {
      id: card
      width: Math.min(Style.space(1120), window.width - Style.gapsOut * 4)
      height: Math.min(content.implicitHeight + contentTopInset + contentBottomInset, window.height - Style.gapsOut * 4)
      anchors.centerIn: parent
      color: root.background
      radius: Style.cornerRadius
      borderSpec: root.borderSpec
      padding: Style.spacing.panelPadding

      // The shell's card timing: a fade with a 1.5 % rise, so the studio settles instead of zooming.
      opacity: root.opened ? 1 : 0
      scale: root.opened ? 1 : 0.985
      Behavior on opacity { enabled: !root.reducedMotion; NumberAnimation { duration: 140; easing.type: Easing.OutCubic } }
      Behavior on scale { enabled: !root.reducedMotion; NumberAnimation { duration: 160; easing.type: Easing.OutCubic } }

      MouseArea { anchors.fill: parent; onClicked: {} }

      FocusScope {
        id: keyScope
        anchors.fill: parent
        anchors.topMargin: card.contentTopInset
        anchors.rightMargin: card.contentRightInset
        anchors.bottomMargin: card.contentBottomInset
        anchors.leftMargin: card.contentLeftInset
        focus: true
        Keys.onPressed: function(event) { if (root.handleKey(event)) event.accepted = true }

        Column {
          id: content
          width: parent.width
          spacing: Style.spacing.panelGap

          // ---------- header ----------
          Item {
            width: parent.width
            implicitHeight: Math.max(title.implicitHeight, Style.space(26))
            Row {
              id: title
              anchors.left: parent.left
              anchors.verticalCenter: parent.verticalCenter
              spacing: Style.spacing.lg
              Text {
                anchors.verticalCenter: parent.verticalCenter
                text: Model.GLYPH.fan
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.heading
              }
              Text {
                anchors.verticalCenter: parent.verticalCenter
                text: "Penna"
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.heading
                font.bold: true
              }
              Text {
                anchors.verticalCenter: parent.verticalCenter
                text: root.activeProfile ? "· " + root.activeProfile.label + " profile" : ""
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
              }
            }
            Row {
              anchors.right: parent.right
              anchors.verticalCenter: parent.verticalCenter
              spacing: Style.spacing.xl
              Row {
                visible: client.dirty
                anchors.verticalCenter: parent.verticalCenter
                spacing: Style.spacing.md
                Rectangle { anchors.verticalCenter: parent.verticalCenter; width: Style.space(7); height: width; radius: width / 2; color: root.accent }
                Text {
                  text: "Unsaved changes, running live"
                  color: root.accent
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.bodySmall
                }
              }
              Text {
                anchors.verticalCenter: parent.verticalCenter
                text: "esc"
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }
            }
          }

          // ---------- body ----------
          Item {
            width: parent.width
            implicitHeight: Math.max(leftCol.implicitHeight, centre.implicitHeight, rightCol.implicitHeight)

            // Controls
            Column {
              id: leftCol
              width: Style.space(250)
              spacing: Style.spacing.md
              PanelSectionHeader { text: "CONTROLS"; foreground: root.foreground; fontFamily: root.fontFamily }
              Repeater {
                model: root.controlKeys
                ControlCard {
                  required property var modelData
                  width: leftCol.width
                  control: Model.byKey(root.liveControls, modelData)
                  tick: root.tick
                  nowSec: root.service ? root.service.lastTickAt : 0
                  selected: modelData === root.selectedKey
                  hasCursor: root.focusArea === "controls" && modelData === root.selectedKey
                  foreground: root.foreground
                  accent: root.accent
                  urgent: root.urgent
                  fontFamily: root.fontFamily
                  tickMs: root.tickMs
                  live: root.live
                  reducedMotion: root.reducedMotion
                  onClicked: {
                    root.focusArea = "controls"
                    root.selectControl(String(modelData))
                  }
                }
              }
              Text {
                width: leftCol.width
                visible: root.tick && Array.isArray(root.tick.firmware) && root.tick.firmware.length > 0
                wrapMode: Text.Wrap
                text: root.tick && root.tick.firmware ? "Firmware runs " + root.tick.firmware.map(function(f) { return f.label }).join(", ") + "." : ""
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }
            }

            // Editor
            Column {
              id: centre
              anchors.left: leftCol.right
              anchors.right: rightCol.left
              anchors.leftMargin: Style.space(18)
              anchors.rightMargin: Style.space(18)
              spacing: Style.spacing.lg

              Item {
                width: parent.width
                implicitHeight: Math.max(curveName.implicitHeight, assign.implicitHeight)
                Text {
                  id: curveName
                  anchors.left: parent.left
                  anchors.verticalCenter: parent.verticalCenter
                  text: root.selectedCurve ? root.selectedCurve.label : (root.selectedLive ? root.selectedLive.label + " follows its firmware" : "")
                  color: root.foreground
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.title
                  font.bold: true
                }
                Dropdown {
                  id: assign
                  anchors.right: parent.right
                  anchors.verticalCenter: parent.verticalCenter
                  width: Style.space(200)
                  label: "Curve"
                  showLabel: false
                  enabled: root.canWrite && root.selectedKey !== ""
                  options: root.curveOptionList
                  value: root.selectedCurveId
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  onChanged: function(v) { if (v !== root.selectedCurveId) client.assign(root.selectedKey, v) }
                }
              }

              Row {
                spacing: Style.spacing.md
                visible: root.selectedCurve !== null
                Repeater {
                  model: root.chipList
                  Rectangle {
                    required property var modelData
                    visible: modelData !== ""
                    width: chipText.implicitWidth + Style.spacing.md * 2
                    height: chipText.implicitHeight + Style.spacing.xs * 2
                    color: Util.alpha(root.foreground, 0.04)
                    border.width: 1
                    border.color: Util.alpha(root.foreground, 0.4)
                    Text {
                      id: chipText
                      anchors.centerIn: parent
                      text: modelData
                      color: root.foreground
                      font.family: root.fontFamily
                      font.pixelSize: Style.font.caption
                    }
                  }
                }
              }

              Rectangle {
                width: parent.width
                height: Style.space(400)
                color: "transparent"
                border.width: root.focusArea === "editor" ? Style.space(2) : 1
                border.color: root.focusArea === "editor" ? root.accent : Util.alpha(root.foreground, 0.12)

                CurveEditor {
                  id: editor
                  anchors.fill: parent
                  anchors.margins: Style.spacing.lg
                  visible: root.selectedCurve !== null && root.selectedCurve.kind !== "mix"
                  curve: root.selectedCurve
                  control: root.selectedInfo
                  live: root.selectedCurveLive
                  tickMs: root.tickMs
                  reducedMotion: root.reducedMotion
                  running: root.live
                  editable: root.canWrite
                  keyboardActive: root.focusArea === "editor"
                  foreground: root.foreground
                  accent: root.accent
                  fontFamily: root.fontFamily
                  onEdited: function(curve, final) { root.onEdited(curve, final) }
                }

                MixView {
                  anchors.fill: parent
                  anchors.margins: Style.spacing.xl
                  visible: root.selectedCurve !== null && root.selectedCurve.kind === "mix"
                  curve: root.selectedCurve
                  bodyCurves: root.body ? root.body.curves : []
                  liveCurves: root.tick && root.tick.curves ? root.tick.curves : []
                  usedBy: root.labelsOf(root.selectedCurve ? root.selectedCurve.used_by : [])
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  tickMs: root.tickMs
                  live: root.live
                  reducedMotion: root.reducedMotion
                }

                Text {
                  anchors.centerIn: parent
                  width: parent.width * 0.7
                  horizontalAlignment: Text.AlignHCenter
                  wrapMode: Text.Wrap
                  visible: root.selectedCurve === null
                  text: !root.connected ? "Penna isn't running. Start it, and the studio picks up where it was."
                      : root.selectedKey === "" ? "No fan is adopted yet. Adopting one saves the way back first."
                      : "The firmware runs this fan. Choose a curve above to adopt it."
                  color: root.dim
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.body
                }
              }

              Item {
                width: parent.width
                implicitHeight: Math.max(nowText.implicitHeight, whatText.implicitHeight)
                Text {
                  id: nowText
                  anchors.left: parent.left
                  text: {
                    var c = root.selectedCurveLive
                    if (!c || c.out === null || c.out === undefined) return ""
                    return (c.smoothed !== null && c.smoothed !== undefined ? Number(c.smoothed).toFixed(1) + " °C → " : "") + Math.round(Number(c.out)) + " %"
                  }
                  color: root.foreground
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.bodySmall
                  font.features: ({ "tnum": 1 })
                }
                Text {
                  id: whatText
                  anchors.right: parent.right
                  anchors.left: nowText.right
                  anchors.leftMargin: Style.spacing.xl
                  horizontalAlignment: Text.AlignRight
                  elide: Text.ElideRight
                  text: Model.inspector(root.selectedCurveLive, root.selectedInfo, root.selectedLive, root.tick)
                  color: root.dim
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.bodySmall
                }
              }

              Text {
                width: parent.width
                text: Model.limitsLine(root.selectedInfo)
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }
            }

            // Sensors
            Column {
              id: rightCol
              anchors.right: parent.right
              width: Style.space(250)
              spacing: Style.spacing.lg
              PanelSectionHeader { text: "SENSORS"; foreground: root.foreground; fontFamily: root.fontFamily }
              Repeater {
                id: sensorRepeater
                model: root.sensorKeys
                SensorRow {
                  required property var modelData
                  readonly property var s: Model.byKey(root.plausibleSensors, modelData) || ({ label: "", celsius: null })
                  width: rightCol.width
                  key: modelData
                  name: s.virtual ? s.label : Model.shortName(s)
                  detail: s.virtual || String(s.label).toLowerCase() === Model.shortName(s).toLowerCase() ? "" : s.label
                  celsius: s.celsius === null || s.celsius === undefined ? NaN : Number(s.celsius)
                  lo: Model.scale(s)[0]
                  hi: Model.scale(s)[1]
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  tickMs: root.tickMs
                  live: root.live
                  reducedMotion: root.reducedMotion
                }
              }
              Text {
                width: rightCol.width
                text: "5 minutes · fixed scales"
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }
              Text {
                width: rightCol.width
                visible: root.coldSensors.length > 0
                wrapMode: Text.Wrap
                text: "Not shown: " + root.coldSensors.map(function(s) { return s.device + " · " + s.label }).join(", ") + ", below room temperature."
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }
            }
          }

          // ---------- action bar ----------
          PanelSeparator { foreground: root.foreground }

          Item {
            width: parent.width
            implicitHeight: Math.max(buttons.implicitHeight, hint.implicitHeight)

            Text {
              id: hint
              anchors.left: parent.left
              anchors.right: buttons.left
              anchors.rightMargin: Style.spacing.xl
              anchors.verticalCenter: parent.verticalCenter
              elide: Text.ElideRight
              text: client.error !== "" ? client.error
                  : root.closeArmed ? "Unsaved changes. Esc again discards them and closes; ctrl+s keeps them."
                  : client.lockedByOther ? "Another studio has unsaved changes."
                  : !root.canWrite && client.ready ? (root.service && root.service.writeWhy !== "" ? root.service.writeWhy : Model.writeBlockedReason(true, false))
                  : root.notice !== "" ? root.notice
                  : root.hints
              color: client.error !== "" || root.closeArmed ? root.urgent : root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
            }

            Row {
              id: buttons
              anchors.right: parent.right
              anchors.verticalCenter: parent.verticalCenter
              spacing: Style.spacing.md
              Repeater {
                model: root.actions
                Button {
                  required property var modelData
                  required property int index
                  text: modelData.label
                  bordered: true
                  active: modelData.id === "save" && modelData.enabled
                  opacity: modelData.enabled ? 1 : 0.45
                  tooltipText: modelData.tip
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  hasCursor: root.focusArea === "actions" && root.actionIndex === index
                  onClicked: root.activateAction(index)
                  onHovered: function(h) { if (h) { root.focusArea = "actions"; root.actionIndex = index } }
                }
              }
            }
          }
        }
      }
    }
  }
}
