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
  // The profile the studio shows and edits, which needn't be the one running: setup's curves fill three while Defaults may
  // run (§8.3, §11.3 call 9). Choosing one never changes what runs.
  property string viewChoice: ""
  readonly property string viewId: Model.viewProfile(body, viewChoice)
  readonly property var viewedProfile: body ? find(body.profiles, "id", viewId) : null
  readonly property var profileTabList: Model.profileTabs(body)
  readonly property string selectedCurveId: viewedProfile && viewedProfile.assign && viewedProfile.assign[selectedKey] ? String(viewedProfile.assign[selectedKey]) : "firmware"
  readonly property var selectedCurve: body ? find(body.curves, "id", selectedCurveId) : null
  readonly property var selectedCurveLive: tick ? find(tick.curves, "id", selectedCurveId) : null
  readonly property var plausibleSensors: Model.sensors(tick).filter(function(s) {
    return !s.below_ambient && (s.asleep === true || (s.celsius !== null && s.celsius !== undefined))
  })
  readonly property var coldSensors: Model.sensors(tick).filter(function(s) { return s.below_ambient })

  // A drag sends drafts ten times a second and each reply is a whole studio body; lists built from it are replaced only
  // when their content changes, so the dropdown and the chips aren't rebuilt under the hand.
  property var curveOptionList: []
  property var chipList: []
  property var anchorList: []
  function syncLists() {
    var o = Model.curveOptions(body, selectedInfo)
    if (JSON.stringify(o) !== JSON.stringify(curveOptionList)) curveOptionList = o
    var c = selectedCurve
    var chips = !c ? []
              : c.kind === "graph" ? ["Graph", c.source_label || "", "Response " + c.response + " s", "Hysteresis " + c.hysteresis + " °C"]
              : c.kind === "trigger" ? ["Trigger", c.source_label || "", "On at " + c.trigger.on + " °C, off at " + c.trigger.off + " °C"]
              : c.kind === "mix" ? ["Mix", c.mix.op]
              : ["Flat"]
    // Setup made it, and it isn't saved yet (§11.3 call 9).
    if (c && Model.isGenerated(body, c.id)) chips.unshift("Generated")
    if (JSON.stringify(chips) !== JSON.stringify(chipList)) chipList = chips
    var a = Model.anchorMarks(body, c)
    if (JSON.stringify(a) !== JSON.stringify(anchorList)) anchorList = a
  }
  onBodyChanged: syncLists()
  onSelectedCurveChanged: syncLists()
  // The curve choices depend on the fan: one whose firmware runs its curve takes fewer (§3.5).
  onSelectedInfoChanged: syncLists()

  // Repeater keys: replaced only when the set of rows changes, so cards and sparklines survive ticks (§8.6). Cards sit under
  // a heading per device, which names its tier once (design review 03, call 10).
  property var controlKeys: []
  property var sensorKeys: []
  property var deviceGroupList: []
  function syncKeys() {
    var c = Model.keysOf(liveControls)
    if (!Model.sameKeys(c, controlKeys)) controlKeys = c
    var s = Model.keysOf(plausibleSensors)
    if (!Model.sameKeys(s, sensorKeys)) sensorKeys = s
    var g = Model.deviceGroups(liveControls, tick)
    if (JSON.stringify(g) !== JSON.stringify(deviceGroupList)) deviceGroupList = g
  }

  // ---------------------------------------------------------- setup (§5, design review 03 call 4)
  readonly property var setupLive: tick && tick.setup ? tick.setup : null
  property bool setupScreen: false      // the start screen, before a run
  property bool setupDismissed: false   // Done: back to the curves while the run's results stay on the connection
  readonly property bool setupShown: setupScreen || (setupLive !== null && !setupDismissed)
  readonly property bool setupRunning: setupLive !== null && !setupLive.stopped && setupLive.step !== "review"

  function startSetup() {
    client.setupStart(setupView.loadCheck, function(reply) {
      if (reply.ok) {
        root.setupScreen = false
        root.setupDismissed = false
      } else {
        root.say(reply.error)
      }
    })
  }

  // Confirmed roles open the curves setup proposes in the editor (§8.3). A fan on a pump header confirmed as a case fan is
  // measured below 50 % first, and the editor opens when that is done.
  property string curvesWait: ""        // "remeasure", then "remeasuring", while that fan is measured
  function confirmRoles() {
    client.setupRoles(setupView.chosenRoles(), function(reply) {
      if (!reply.ok) {
        root.say(reply.error)
        return
      }
      var text = (reply.result && reply.result.text ? String(reply.result.text) : "Roles confirmed") + "."
      if (text.indexOf("measuring") >= 0) {
        root.curvesWait = "remeasure"
        root.say(text)
      } else {
        root.showCurves(text)
      }
    })
  }

  function showCurves(text) {
    curvesWait = ""
    var p = setupLive ? setupLive.proposed : null
    if (p && p.refused) {
      say(p.refused)
      return
    }
    viewChoice = ""
    setupScreen = false
    setupDismissed = true
    client.refresh()
    var n = p && p.curves ? p.curves.length : 0
    say(text + (n ? " Setup's " + n + " curves are marked Generated until you save them." : ""))
  }

  onSetupLiveChanged: {
    if (!setupLive || setupLive.stopped) curvesWait = ""
    else if (curvesWait === "remeasure" && setupLive.step !== "review") curvesWait = "remeasuring"
    else if (curvesWait === "remeasuring" && setupLive.step === "review") showCurves("Measured.")
  }

  // A drifting fan's Recalibrate: setup for that fan alone, on this connection (§5.6).
  function recalibrate(key) {
    client.request("setup.start", { controls: [key] }, function(reply) {
      if (reply.ok) {
        root.setupScreen = false
        root.setupDismissed = false
      } else {
        root.say(reply.error)
      }
    })
  }

  function leaveSetup() {
    setupScreen = false
    setupDismissed = true
    client.refresh()
  }

  // The action bar's cursor lands on the main button whenever setup's view changes.
  onActionsChanged: actionIndex = Math.max(0, actions.length - 1)

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

  // While setup shows, the action bar is setup's: Stop and hand back sits where Save does (§11.3 call 4).
  readonly property var actions: !setupShown ? [
    { id: "setup", label: "Set up fans…", enabled: canWrite && !client.dirty, tip: client.dirty ? "Save or discard your changes first" : "" },
    { id: "discard", label: "Discard", enabled: client.dirty, tip: "" },
    { id: "save", label: Model.saveLabel(body), enabled: client.dirty, tip: "" }
  ] : setupLive === null ? [
    { id: "cancel", label: "Cancel", enabled: true, tip: "" },
    { id: "start", label: "Start setup", enabled: canWrite, tip: "", primary: true }
  ] : setupRunning ? [
    { id: "stop", label: "Stop and hand back", enabled: canWrite, tip: "" }
  ] : setupLive.stopped ? [
    { id: "again", label: "Start again", enabled: canWrite, tip: "", primary: true },
    { id: "done", label: "Done", enabled: true, tip: "" }
  ] : [
    { id: "confirm", label: "Confirm roles", enabled: canWrite, tip: "", primary: true },
    { id: "done", label: "Done", enabled: true, tip: "" }
  ]

  function open(payloadJson) {
    var payload = ({})
    try { payload = JSON.parse(payloadJson || "{}") || ({}) } catch (e) { payload = ({}) }
    setupScreen = payload.setup === true
    setupDismissed = false
    screenName = focusedName()
    targetScreen = screenOverride || pickScreen()
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

  // Unsaved changes, or a setup run, ask once: the first Esc arms, the second discards (or stops setup) and closes.
  function requestClose() {
    if ((client.dirty || setupRunning) && !closeArmed) {
      closeArmed = true
      return
    }
    close()
  }

  function save() {
    if (!client.dirty) return
    flushPending()
    var generated = Model.generatedDraft(body)
    client.save(function(reply) {
      if (reply.ok) {
        root.closeArmed = false
        root.viewChoice = ""
        root.say(generated ? "Saved. Penna runs every fan on Balanced; switch profiles in the popup."
                           : reply.result && reply.result.text ? reply.result.text : "Saved.")
      }
    })
  }

  function discard() {
    if (!client.dirty) return
    pendingCurve = null
    var generated = Model.generatedDraft(body)
    client.discard(function(reply) {
      root.closeArmed = false
      editor.syncFromCurve()
      if (reply.ok) root.say(generated ? "Not saved: every fan runs as it did before setup. What setup measured is kept." : "Back to the saved curves.")
    })
  }

  // p: the next profile to show.
  function cycleView() {
    var tabs = profileTabList
    if (!tabs.length) return
    var i = tabs.map(function(t) { return t.value }).indexOf(viewId)
    viewChoice = tabs[(i + 1) % tabs.length].value
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
    else if (a.id === "setup") { setupScreen = true; setupDismissed = false }
    else if (a.id === "cancel") setupScreen = false
    else if (a.id === "start" || a.id === "again") startSetup()
    else if (a.id === "stop") client.setupStop(function(reply) { if (!reply.ok) root.say(reply.error) })
    else if (a.id === "confirm") confirmRoles()
    else if (a.id === "done") leaveSetup()
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
    if (setupShown) {
      // Setup's view has one area: the action bar. On the start screen, c turns the load check on or off.
      if (k === Qt.Key_C && setupScreen && setupLive === null) { setupView.loadCheck = !setupView.loadCheck; return true }
      if (k === Qt.Key_H || k === Qt.Key_Left) { actionIndex = Math.max(0, actionIndex - 1); return true }
      if (k === Qt.Key_L || k === Qt.Key_Right) { actionIndex = Math.min(actions.length - 1, actionIndex + 1); return true }
      if (k === Qt.Key_Return || k === Qt.Key_Enter || k === Qt.Key_Space) { activateAction(actionIndex); return true }
      return false
    }
    if (k === Qt.Key_P && focusArea !== "editor") { cycleView(); return true }
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
      if (root.setupShown) setupView.ticked()
    }
    function onHistoryReplaced() { if (root.opened) root.pushAll() }
  }

  StudioClient { id: client }

  function focusedName() { return Hyprland.focusedMonitor ? String(Hyprland.focusedMonitor.name) : "" }

  function pickScreen() {
    var names = []
    for (var i = 0; i < Quickshell.screens.length; i++) names.push(String(Quickshell.screens[i].name))
    var at = Model.screenIndex(names, screenName, focusedName())
    return at < 0 ? null : Quickshell.screens[at]
  }

  readonly property string hints: setupShown ? (setupRunning ? "Setup belongs to this window: closing it stops setup and hands every fan back."
                                              : setupScreen && setupLive === null ? "c load check · h/l action · ↵ choose · esc close"
                                              : "h/l action · ↵ choose · esc close")
                                : focusArea === "controls" ? "j/k control · ↵ edit its curve · p profile · ⇥ editor · esc close"
                                : focusArea === "editor" ? "[ ] point · h/l °C · j/k % · ⇧ ×5 · a add · x delete · ⇥ actions · ctrl+s save"
                                : "h/l action · ↵ choose · ⇥ controls · esc close"

  // ---------------------------------------------------------- window
  // A monitor that drops out takes the studio's surface with it, while the studio stays open and its connection keeps setup and
  // the draft: when the screens change, the studio goes back to the screen it opened on, or the focused one, and is mapped there
  // afresh (§8.3).
  property string screenName: ""
  property bool remapping: false

  Connections {
    target: Quickshell
    function onScreensChanged() { if (root.opened && !root.screenOverride) screenSettle.restart() }
  }

  // A monitor that blinks goes and comes back within a second: the screens settle, then the studio moves once.
  Timer {
    id: screenSettle
    interval: 250
    onTriggered: {
      var next = root.pickScreen()
      if (next !== null && next === root.targetScreen) return
      root.targetScreen = next
      root.remapping = true
    }
  }

  // Unmapped for a beat, so the compositor makes a new surface rather than keeping none.
  Timer {
    interval: 50
    running: root.remapping
    onTriggered: {
      root.remapping = false
      Qt.callLater(function() { keyScope.forceActiveFocus() })
    }
  }

  PanelWindow {
    id: window
    visible: (root.opened || card.opacity > 0) && !root.remapping
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
                visible: root.setupShown
                text: "· Setting up fans"
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
              }
              ProfilePills {
                anchors.verticalCenter: parent.verticalCenter
                visible: !root.setupShown && root.profileTabList.length > 0
                options: root.profileTabList
                value: root.viewId
                foreground: root.foreground
                fontFamily: root.fontFamily
                onChanged: function(v) { root.viewChoice = v }
              }
              Text {
                anchors.verticalCenter: parent.verticalCenter
                visible: !root.setupShown && root.body !== null && root.viewId !== "" && root.viewId !== root.body.profile
                text: root.body ? Model.profileLabel(root.body) + " runs" : ""
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.bodySmall
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
                  text: Model.draftLine(root.body)
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

          // ---------- setup ----------
          SetupView {
            id: setupView
            visible: root.setupShown
            width: parent.width
            setup: root.setupLive
            tick: root.tick
            history: root.service ? root.service.history : ({})
            starting: root.setupScreen && root.setupLive === null
            canWrite: root.canWrite
            foreground: root.foreground
            accent: root.accent
            urgent: root.urgent
            fontFamily: root.fontFamily
            tickMs: root.tickMs
            live: root.live
            reducedMotion: root.reducedMotion
            onIdentifyRequested: function(key) { client.identify(key, function(r) { if (!r.ok) root.say(r.error) }) }
            onRenameRequested: function(key, label) { client.label(key, label, function(r) { root.say(r.ok ? r.result.text + "." : r.error) }) }
          }

          // ---------- body ----------
          Item {
            visible: !root.setupShown
            width: parent.width
            implicitHeight: visible ? Math.max(leftCol.implicitHeight, centre.implicitHeight, rightCol.implicitHeight) : 0

            // Controls
            Column {
              id: leftCol
              width: Style.space(250)
              spacing: Style.spacing.md
              PanelSectionHeader { text: "CONTROLS"; foreground: root.foreground; fontFamily: root.fontFamily }
              Repeater {
                model: root.deviceGroupList
                Column {
                  id: group
                  required property var modelData
                  width: leftCol.width
                  spacing: Style.spacing.md
                  Item {
                    width: parent.width
                    implicitHeight: headText.implicitHeight
                    Text {
                      id: headText
                      anchors.left: parent.left
                      anchors.right: tierText.left
                      elide: Text.ElideRight
                      text: group.modelData.heading
                      color: root.dim
                      font.family: root.fontFamily
                      font.pixelSize: Style.font.caption
                      font.bold: true
                    }
                    Text {
                      id: tierText
                      anchors.right: parent.right
                      text: group.modelData.tier.toUpperCase()
                      color: group.modelData.note !== "" ? root.foreground : root.dim
                      font.family: root.fontFamily
                      font.pixelSize: Style.font.caption
                      font.bold: true
                    }
                  }
                  Text {
                    visible: group.modelData.note !== ""
                    width: parent.width
                    wrapMode: Text.Wrap
                    text: group.modelData.note
                    color: root.dim
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.caption
                  }
                  Repeater {
                    model: group.modelData.keys
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
                      onRecalibrateRequested: function(key) { root.recalibrate(key) }
                    }
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
                  onChanged: function(v) { if (v !== root.selectedCurveId) client.assign(root.selectedKey, v, root.viewId) }
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
                    readonly property bool generated: modelData === "Generated"
                    color: generated ? Util.alpha(root.accent, 0.12) : Util.alpha(root.foreground, 0.04)
                    border.width: 1
                    border.color: generated ? root.accent : Util.alpha(root.foreground, 0.4)
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
                  marks: root.anchorList
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
                  text: !root.connected && root.service && root.service.mismatch !== "" ? root.service.mismatch
                      : !root.connected ? "Penna isn't running. Start it, and the studio picks up where it was."
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
                  asleep: s.asleep === true
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
                  active: (modelData.id === "save" || modelData.primary === true) && modelData.enabled
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
