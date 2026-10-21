import QtQuick
import Quickshell
import qs.Ui
import qs.Commons
import "Model.js" as Model
import "components"

// Bar widget + popup (DESIGN.md §8.1–8.2): a glyph that reports health, and the daily controls. Everything heavier lives
// in the studio (M3).
//
// Cursor model follows the first-party panels and Candela: root owns cursorActive + focusSection + selectedIndex; every
// target binds hasCursor and reports hover, so keyboard and pointer share one highlight.
Panel {
  id: root
  // The bar overwrites moduleName with the configured id as soon as the widget mounts; keep this literal in sync with
  // manifest.json's "id" by hand.
  moduleName: "io.github.pushmotta.penna"
  manageIpc: false

  readonly property color fg: bar ? bar.foreground : Color.foreground
  readonly property color urgentColor: bar ? bar.urgent : Color.urgent
  readonly property color accentColor: Color.accent
  readonly property color dimColor: Qt.darker(fg, 1.4)
  readonly property string fam: bar ? bar.fontFamily : Style.font.family

  readonly property var service: bar && bar.shell ? bar.shell.serviceFor(root.moduleName) : null
  readonly property var tick: service ? service.tick : null
  readonly property bool connected: service ? service.connected : false
  readonly property bool canWrite: service ? service.canWrite : false
  readonly property bool reducedMotion: service ? service.reducedMotion : false
  readonly property int tickMs: service ? Math.round(service.tickSeconds * 1000) : 1000
  readonly property bool live: root.opened && root.connected

  readonly property var barState: Model.barState(tick, connected)
  readonly property var heroState: Model.hero(tick, connected, nowSec, service ? service.lastTickAt : 0)
  readonly property var tempRows: Model.tempRows(tick)
  readonly property var controlsList: Model.adopted(tick)
  readonly property var firmwareLines: Model.firmwareLines(tick)
  readonly property var unreachableLines: Model.unreachableLines(tick)
  readonly property var notes: tick && Array.isArray(tick.notes) ? tick.notes : []
  readonly property var profileOptions: Model.profileOptions(tick)

  // Repeater keys: replaced only when the set of rows changes, so rows survive ticks and keep their glides.
  property var tempKeys: []
  property var controlKeys: []
  function syncKeys() {
    var t = Model.keysOf(tempRows)
    if (!Model.sameKeys(t, tempKeys)) tempKeys = t
    var c = Model.keysOf(controlsList)
    if (!Model.sameKeys(c, controlKeys)) controlKeys = c
  }
  Component.onCompleted: syncKeys()

  // Optimistic choices: shown at once, confirmed (or rolled back with the reason) when the daemon answers (§8.6).
  property string pendingProfile: ""
  property var pendingFull: null
  readonly property string shownProfile: pendingProfile !== "" ? pendingProfile : (tick ? String(tick.profile) : "")
  readonly property bool shownFull: pendingFull !== null ? pendingFull : (tick ? tick.full_speed === true : false)

  property string flash: ""
  property real nowSec: Date.now() / 1000

  // The bar's optional temperature: whole degrees, at most once a tick, never animated (review call 2).
  readonly property string temperatureKey: String(setting("temperature", ""))
  property real barTemp: NaN

  function chooseProfile(id) {
    if (!canWrite || id === shownProfile) return
    pendingProfile = id
    service.setProfile(id, function(reply) {
      if (reply.ok === false) {
        root.pendingProfile = ""
        root.say(reply.error)
      }
    })
  }

  function toggleFullSpeed() {
    if (!canWrite) return
    var on = !shownFull
    pendingFull = on
    service.setFullSpeed(on, function(reply) {
      if (reply.ok === false) {
        root.pendingFull = null
        root.say(reply.error)
      }
    })
  }

  function retryControl(key) {
    service.retry(key, function(reply) { if (reply.ok === false) root.say(reply.error) })
  }

  function say(text) {
    flash = String(text || "")
    flashTimer.restart()
  }

  function pushAll() {
    for (var i = 0; i < tempRepeater.count; i++) {
      var row = tempRepeater.itemAt(i)
      if (!row) continue
      row.values = service && service.history[row.key] ? service.history[row.key] : []
      row.push()
    }
  }

  Timer {
    id: flashTimer
    interval: 6000
    onTriggered: root.flash = ""
  }

  // "Last update 12 s ago" keeps counting while the daemon is away and the popup is open.
  Timer {
    interval: 1000
    repeat: true
    running: root.opened
    onTriggered: root.nowSec = Date.now() / 1000
  }

  Connections {
    target: root.service
    function onTicked() {
      var t = root.tick
      root.syncKeys()
      root.nowSec = Date.now() / 1000
      if (root.pendingProfile !== "" && t && t.profile === root.pendingProfile) root.pendingProfile = ""
      if (root.pendingFull !== null && t && t.full_speed === root.pendingFull) root.pendingFull = null
      if (root.temperatureKey !== "") {
        var s = Model.sensors(t).filter(function(x) { return x.key === root.temperatureKey })[0]
        root.barTemp = s && s.celsius !== null && s.celsius !== undefined ? Model.deadband(root.barTemp, Number(s.celsius), 1) : NaN
      }
      if (root.opened) root.pushAll()
    }
    function onHistoryReplaced() { if (root.opened) root.pushAll() }
  }

  // ---------------------------------------------------------- cursor model
  property string focusSection: "profiles"
  property int selectedIndex: -1
  property bool cursorActive: false

  readonly property var retryKeys: controlsList.filter(function(c) { return canWrite && (c.state === "contested" || c.state === "fault") })
                                               .map(function(c) { return c.key })
  readonly property var sections: {
    var list = []
    if (canWrite && profileOptions.length) list.push("profiles")
    if (retryKeys.length) list.push("retry")
    list.push("actions")
    return list
  }

  function sectionCount(section) {
    if (section === "profiles") return profileOptions.length
    if (section === "retry") return retryKeys.length
    if (section === "actions") return 2
    return 0
  }

  function defaultIndex(section) {
    if (section === "profiles") {
      for (var i = 0; i < profileOptions.length; i++) if (profileOptions[i].value === shownProfile) return i
    }
    return 0
  }

  function moveCursor(dy) {
    var i = sections.indexOf(focusSection)
    if (i < 0) i = 0
    else i = Math.max(0, Math.min(sections.length - 1, i + dy))
    focusSection = sections[i]
    selectedIndex = (focusSection === "retry" && dy < 0) ? sectionCount("retry") - 1 : defaultIndex(focusSection)
  }

  function moveCursorH(dx) {
    if (focusSection === "retry") return moveCursor(dx)
    var n = sectionCount(focusSection)
    selectedIndex = Math.max(0, Math.min(n - 1, selectedIndex + dx))
  }

  function activateCursor() {
    if (focusSection === "profiles" && profileOptions[selectedIndex]) chooseProfile(profileOptions[selectedIndex].value)
    else if (focusSection === "retry" && retryKeys[selectedIndex]) retryControl(retryKeys[selectedIndex])
    else if (focusSection === "actions" && selectedIndex === 0) toggleFullSpeed()
    else if (focusSection === "actions" && selectedIndex === 1) openStudio()
  }

  function openStudio() {
    close()
    if (bar && bar.shell) bar.shell.summon(root.moduleName, "{}")
  }

  function hoverInto(section, index) {
    cursorActive = true
    focusSection = section
    selectedIndex = index
  }

  onOpenedChanged: {
    if (!opened) return
    cursorActive = false
    focusSection = sections[0]
    selectedIndex = defaultIndex(focusSection)
    nowSec = Date.now() / 1000
    Qt.callLater(pushAll)
  }

  implicitWidth: temperatureKey !== "" ? textButton.implicitWidth : glyphButton.implicitWidth
  implicitHeight: temperatureKey !== "" ? textButton.implicitHeight : glyphButton.implicitHeight

  // ---------------------------------------------------------- bar button
  BarIconButton {
    id: glyphButton
    anchors.fill: parent
    visible: root.temperatureKey === ""
    bar: root.bar
    text: root.barState.glyph
    active: root.barState.tone === "urgent" || root.barState.tone === "accent"
    activeColor: root.barState.tone === "urgent" ? root.urgentColor : root.accentColor
    dimmed: root.barState.tone === "dim"
    tooltipText: ""
    onPressed: function(b) { root.toggle() }
  }

  WidgetButton {
    id: textButton
    anchors.fill: parent
    visible: root.temperatureKey !== ""
    bar: root.bar
    text: root.barState.glyph + (isNaN(root.barTemp) ? "" : " " + Math.round(root.barTemp) + "°")
    active: glyphButton.active
    activeColor: glyphButton.activeColor
    dimmed: glyphButton.dimmed
    onPressed: function(b) { root.toggle() }
  }

  // ---------------------------------------------------------- popup
  KeyboardPanel {
    id: panel
    anchorItem: root.temperatureKey !== "" ? textButton : glyphButton
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(380))
    contentHeight: panel.fittedContentHeight(panelColumn.implicitHeight)

    // A section appearing (an alert, a fold) eases the card at the shell's timing instead of jumping (§8.6).
    Behavior on contentHeight { enabled: !root.reducedMotion; NumberAnimation { duration: 140; easing.type: Easing.OutCubic } }

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      onMoveRequested: function(dx, dy) {
        if (!root.cursorActive) {
          root.cursorActive = true
          return
        }
        if (dy !== 0) root.moveCursor(dy)
        else if (dx !== 0) root.moveCursorH(dx)
      }
      onActivateRequested: if (root.cursorActive) root.activateCursor()
      onCloseRequested: root.close()
      onTabRequested: function(direction) { root.switchPanel(direction) }

      Column {
        id: panelColumn
        width: parent.width
        spacing: Style.spacing.panelGap

        // ---------- Hero ----------
        PanelHero {
          width: parent.width
          title: root.heroState.title
          meta: root.heroState.meta !== null
                ? root.heroState.meta
                : Model.normalMeta(Model.fanCount(root.tick), root.tempRows.map(function(r, i) {
                    var item = tempRepeater.itemAt(i)
                    return { name: r.name, text: item ? item.shownText : "" }
                  }))
          foreground: root.fg
          fontFamily: root.fam
          iconComponent: Component {
            Text {
              textFormat: Text.PlainText
              text: root.heroState.glyph
              color: root.heroState.tone === "urgent" ? root.urgentColor
                   : root.heroState.tone === "accent" ? root.accentColor
                   : root.heroState.tone === "dim" ? root.dimColor : root.fg
              font.family: root.fam
              font.pixelSize: Style.font.display
              Behavior on color { ColorAnimation { duration: 140; easing.type: Easing.OutCubic } }
            }
          }
        }

        // ---------- Profiles ----------
        ButtonGroup {
          visible: root.profileOptions.length > 0
          enabled: root.canWrite
          opacity: root.canWrite ? 1 : 0.5
          options: root.profileOptions
          value: root.shownProfile
          foreground: root.fg
          fontFamily: root.fam
          cursorIndex: root.cursorActive && root.focusSection === "profiles" ? root.selectedIndex : -1
          onChanged: function(v) { root.chooseProfile(v) }
          onHovered: function(index, h) { if (h) root.hoverInto("profiles", index) }
        }

        // ---------- Temperatures ----------
        PanelSeparator { visible: root.tempRows.length > 0; foreground: root.fg }

        Column {
          visible: root.tempRows.length > 0
          width: parent.width
          spacing: Style.spacing.md
          opacity: root.connected ? 1 : 0.55

          PanelSectionHeader { text: "TEMPERATURES"; foreground: root.fg; fontFamily: root.fam }

          Repeater {
            id: tempRepeater
            model: root.tempKeys
            TempRow {
              required property var modelData
              readonly property string key: modelData
              readonly property var row: Model.byKey(root.tempRows, modelData) || ({})
              width: parent.width
              name: row.name || ""
              detail: row.detail || ""
              celsius: row.celsius === null || row.celsius === undefined ? NaN : Number(row.celsius)
              failed: !!row.error
              lo: row.lo || 20
              hi: row.hi || 100
              foreground: root.fg
              urgent: root.urgentColor
              fontFamily: root.fam
              tickMs: root.tickMs
              live: root.live
              reducedMotion: root.reducedMotion
            }
          }
        }

        // ---------- Fans ----------
        PanelSeparator { foreground: root.fg }

        Column {
          width: parent.width
          spacing: Style.spacing.md
          opacity: root.connected ? 1 : 0.55

          PanelSectionHeader { text: "FANS"; foreground: root.fg; fontFamily: root.fam }

          Repeater {
            model: root.controlKeys
            FanRow {
              required property var modelData
              width: parent.width
              control: Model.byKey(root.controlsList, modelData)
              tick: root.tick
              nowSec: root.nowSec
              canWrite: root.canWrite
              foreground: root.fg
              accent: root.accentColor
              urgent: root.urgentColor
              fontFamily: root.fam
              tickMs: root.tickMs
              live: root.live
              reducedMotion: root.reducedMotion
              hasCursor: root.cursorActive && root.focusSection === "retry" && root.retryKeys[root.selectedIndex] === modelData
              onRetryRequested: function(key) { root.retryControl(key) }
              onHovered: function(h) { if (h) root.hoverInto("retry", root.retryKeys.indexOf(modelData)) }
            }
          }

          Fold {
            width: parent.width
            title: "Firmware"
            count: root.firmwareLines.length
            lines: root.firmwareLines
            foreground: root.fg
            fontFamily: root.fam
          }

          Fold {
            width: parent.width
            title: "Not reachable"
            count: root.unreachableLines.length
            lines: root.unreachableLines
            expanded: true
            foreground: root.fg
            fontFamily: root.fam
          }

          Text {
            width: parent.width
            visible: root.notes.length > 0
            text: root.notes.join("\n")
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            color: root.dimColor
            font.family: root.fam
            font.pixelSize: Style.font.caption
          }
        }

        // ---------- Why nothing can change, or what just failed ----------
        Text {
          width: parent.width
          visible: text !== ""
          text: !root.connected
                ? "The fans are back on firmware control. Start Penna with sudo systemctl start pennad"
                  + (root.service && root.service.lastMock ? ", or pennad --mock render-burst --forever while developing." : ".")
                : root.flash !== "" ? root.flash : (root.service && root.service.writeWhy !== "" ? root.service.writeWhy : Model.writeBlockedReason(root.connected, root.canWrite))
          textFormat: Text.PlainText
          wrapMode: Text.Wrap
          color: root.flash !== "" ? root.urgentColor : root.dimColor
          font.family: root.fam
          font.pixelSize: Style.font.caption
        }

        // ---------- Actions ----------
        PanelSeparator { foreground: root.fg }

        Item {
          width: parent.width
          implicitHeight: fullButton.implicitHeight

          Button {
            id: fullButton
            anchors.left: parent.left
            text: "Full speed"
            bordered: true
            active: root.shownFull
            enabled: root.canWrite
            opacity: root.canWrite ? 1 : 0.5
            foreground: root.fg
            fontFamily: root.fam
            hasCursor: root.cursorActive && root.focusSection === "actions" && root.selectedIndex === 0
            onClicked: root.toggleFullSpeed()
            onHovered: function(h) { if (h) root.hoverInto("actions", 0) }
          }

          Button {
            anchors.right: parent.right
            text: "Curves…"
            bordered: true
            foreground: root.fg
            fontFamily: root.fam
            hasCursor: root.cursorActive && root.focusSection === "actions" && root.selectedIndex === 1
            onClicked: root.openStudio()
            onHovered: function(h) { if (h) root.hoverInto("actions", 1) }
          }
        }
      }
    }
  }
}
