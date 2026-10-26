import QtQuick
import qs.Commons
import qs.Ui
import "../Model.js" as Model

// Setup in the studio (DESIGN.md §5, §8.3 as approved in design review 03, call 4). It takes the studio over: the fans on the
// left with their live RPM and step, the rail of steps and one panel per step in the middle, the guards on the right. It ends
// on the review: each fan's role with its reason, a name for it, and Identify; confirming them opens the curves setup
// proposes in the editor (§5.5). The action bar stays the studio's.
Item {
  id: root

  property var setup: null           // tick.setup, or null before a run starts
  property var tick: null
  property bool starting: false      // the start screen: no run yet
  property bool canWrite: false
  property color foreground: Color.foreground
  property color accent: Color.accent
  property color urgent: Color.urgent
  property string fontFamily: Style.font.family
  property int tickMs: 1000
  property bool live: true
  property bool reducedMotion: false
  property var history: ({})        // the service's five minutes of readings, for the load check's chart
  // The start screen's one option (§11.3 call 4): 3 minutes of full CPU load on the proposed curves.
  property bool loadCheck: true

  // The review's choices, kept here until the user confirms them: control → role.
  property var roleChoice: ({})

  signal identifyRequested(string key)
  signal renameRequested(string key, string label)

  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property string step: starting || !setup ? "start" : setup.stopped ? "stopped" : String(setup.step)
  readonly property var rows: Model.setupRows(setup, tick)
  readonly property var review: Model.reviewRows(setup)
  readonly property var load: Model.loadView(setup)

  // Once a tick, from the studio: the load check's chart moves on.
  function ticked() {
    if (step !== "load-check" || !load || !load.source) return
    loadChart.values = history[load.source] || []
    loadChart.push()
  }

  function roleOf(row) { return roleChoice[row.key] !== undefined ? roleChoice[row.key] : row.role }
  function chooseNext(row) {
    var next = ({})
    for (var k in roleChoice) next[k] = roleChoice[k]
    // A dashed chip's first press is its yes; after that each press is the next role.
    next[row.key] = row.confirm && roleChoice[row.key] === undefined ? row.role : Model.nextRole(roleOf(row))
    roleChoice = next
  }
  // What `setup.roles` gets: every suggestion, as chosen.
  function chosenRoles() {
    var out = ({})
    for (var i = 0; i < review.length; i++) out[review[i].key] = roleOf(review[i])
    return out
  }

  implicitHeight: Math.max(fansCol.implicitHeight, centre.implicitHeight, guardsCol.implicitHeight)

  // ---------- the fans ----------
  Column {
    id: fansCol
    width: Style.space(250)
    spacing: Style.spacing.md
    PanelSectionHeader { text: "FANS"; foreground: root.foreground; fontFamily: root.fontFamily }
    Repeater {
      model: root.rows
      Rectangle {
        required property var modelData
        width: fansCol.width
        implicitHeight: rowCol.implicitHeight + Style.spacing.md * 2
        color: Util.alpha(root.foreground, 0.04)
        border.width: 1
        border.color: modelData.hot ? root.accent : Util.alpha(root.foreground, 0.4)
        opacity: modelData.empty ? 0.6 : 1
        Column {
          id: rowCol
          anchors.left: parent.left
          anchors.right: parent.right
          anchors.top: parent.top
          anchors.margins: Style.spacing.md
          spacing: Style.spacing.xs
          Item {
            width: parent.width
            implicitHeight: Math.max(nameText.implicitHeight, rpmText.implicitHeight)
            Text {
              id: nameText
              anchors.left: parent.left
              anchors.right: rpmText.left
              anchors.rightMargin: Style.spacing.md
              elide: Text.ElideRight
              textFormat: Text.StyledText
              text: Model.dimmedLine(modelData.name, modelData.role || "", root.dim, true)
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
              font.bold: true
            }
            LiveNumber {
              id: rpmText
              anchors.right: parent.right
              width: Style.space(80)
              horizontalAlignment: Text.AlignRight
              value: modelData.rpm === null || modelData.rpm === undefined ? NaN : Number(modelData.rpm)
              step: 10
              grouped: true
              suffix: " RPM"
              tickMs: root.tickMs
              live: root.live
              reducedMotion: root.reducedMotion
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
            }
          }
          Item {
            width: parent.width
            implicitHeight: statusText.implicitHeight
            Text {
              textFormat: Text.PlainText
              id: statusText
              anchors.left: parent.left
              anchors.right: dutyText.left
              anchors.rightMargin: Style.spacing.md
              elide: Text.ElideRight
              text: modelData.status
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
            }
            Text {
              textFormat: Text.PlainText
              id: dutyText
              anchors.right: parent.right
              text: modelData.duty === null ? "" : Math.round(modelData.duty) + " %"
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              font.features: ({ "tnum": 1 })
            }
          }
        }
      }
    }
  }

  // ---------- the rail and the step ----------
  Column {
    id: centre
    anchors.left: fansCol.right
    anchors.right: guardsCol.left
    anchors.leftMargin: Style.space(18)
    anchors.rightMargin: Style.space(18)
    spacing: Style.spacing.lg

    Flow {
      width: parent.width
      spacing: Style.spacing.sm
      Repeater {
        model: root.starting ? [] : Model.setupRail(root.setup)
        Rectangle {
          required property var modelData
          width: railText.implicitWidth + Style.spacing.md * 2
          height: railText.implicitHeight + Style.spacing.xs * 2
          color: "transparent"
          border.width: 1
          border.color: modelData.state === "on" ? root.accent : Util.alpha(root.foreground, 0.12)
          Text {
            textFormat: Text.PlainText
            id: railText
            anchors.centerIn: parent
            text: modelData.label
            color: modelData.state === "on" ? root.foreground : root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            font.bold: modelData.state === "on"
          }
        }
      }
    }

    Rectangle {
      width: parent.width
      implicitHeight: Math.max(Style.space(440), panel.implicitHeight + Style.spacing.xl * 2)
      color: "transparent"
      border.width: 1
      border.color: Util.alpha(root.foreground, 0.12)

      Column {
        id: panel
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        anchors.margins: Style.spacing.xl
        spacing: Style.spacing.lg

        // Start: what will happen, and how long it takes.
        Column {
          visible: root.step === "start"
          width: parent.width
          spacing: Style.spacing.lg
          Text { textFormat: Text.PlainText; text: "Set up fans"; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.title; font.bold: true }
          Text {
            textFormat: Text.PlainText
            width: parent.width
            wrapMode: Text.Wrap
            text: "Penna measures every fan: what it reads back, which header it turns, its role, and its speed at every duty. "
                + "Then it proposes curves for Quiet, Balanced and Render, and nothing is saved until you say so."
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
          }
          Repeater {
            model: [["1", "Idle sample", "30 s", "Watch every sensor before any fan is taken."],
                    ["2", "Write test", "5 s", "Take each fan, write 60 % and 80 %, read both back."],
                    ["3", "Pairing", "12 s a fan", "Find which header each control turns, one at a time."],
                    ["4", "Roles", "", "Suggest what each fan does, with the reason."],
                    ["5", "Calibration", "about 3:00", "Measure each fan's speed at every duty, all at once."],
                    ["6", "Load check", root.loadCheck ? "3:00" : "skipped", "Load every CPU thread on the proposed Balanced curves, and see where it levels."],
                    ["7", "Review", "", "Confirm the roles and name the fans, then the proposed curves."]]
            Row {
              required property var modelData
              spacing: Style.spacing.lg
              Text { textFormat: Text.PlainText; width: Style.space(18); text: modelData[0]; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
              Text { textFormat: Text.PlainText; width: Style.space(110); text: modelData[1]; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall; font.bold: true }
              Text { textFormat: Text.PlainText; width: Style.space(84); text: modelData[2]; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
              Text {
                textFormat: Text.PlainText
                width: panel.width - Style.space(18 + 110 + 84) - Style.spacing.lg * 3
                wrapMode: Text.Wrap
                text: modelData[3]
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.bodySmall
              }
            }
          }
          Toggle {
            width: parent.width
            label: "Load check"
            description: "3 minutes of full CPU load at the lowest priority, so other work still comes first. If the CPU comes within "
                       + "5 °C of its limit, the curve's top moves 5 °C earlier and the check runs again."
            checked: root.loadCheck
            enabled: root.canWrite
            foreground: root.foreground
            accent: root.accent
            fontFamily: root.fontFamily
            titleSize: Style.font.bodySmall
            onClicked: root.loadCheck = !root.loadCheck
          }
          Text {
            textFormat: Text.PlainText
            width: parent.width
            wrapMode: Text.Wrap
            text: (root.loadCheck ? "About nine minutes" : "About six minutes") + ", and you will hear the fans. The machine should be idle: "
                + "setup waits while it is busy."
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
          }
        }

        // Idle sample, write test and roles: every sensor, or every fan's own line.
        Repeater {
          model: root.step === "idle" ? Model.sensors(root.tick).filter(function(s) { return !s.below_ambient && s.celsius !== null && s.celsius !== undefined }) : []
          Item {
            required property var modelData
            width: panel.width
            implicitHeight: sName.implicitHeight
            Text {
              textFormat: Text.PlainText
              id: sName
              readonly property string short: Model.shortName(modelData)
              text: short + (String(modelData.label).toLowerCase() === short.toLowerCase() ? "" : " · " + modelData.label)
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
            }
            Text {
              textFormat: Text.PlainText
              anchors.right: parent.right
              text: Number(modelData.celsius).toFixed(1) + " °C"
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
              font.features: ({ "tnum": 1 })
            }
          }
        }
        Repeater {
          model: root.step === "write-test" ? root.rows : []
          Item {
            required property var modelData
            width: panel.width
            implicitHeight: wName.implicitHeight
            Text { textFormat: Text.PlainText; id: wName; text: modelData.name; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
            Text { textFormat: Text.PlainText; anchors.right: parent.right; text: modelData.status; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
          }
        }

        // Pairing: every tach on one scale, the one moving now in the accent (call 6).
        Column {
          visible: root.step === "pairing"
          width: parent.width
          spacing: Style.spacing.sm
          PanelSectionHeader { text: "TACHS · NAMED FROM THE BOARD DATABASE"; foreground: root.foreground; fontFamily: root.fontFamily }
          Repeater {
            model: root.step === "pairing" ? Model.pairBars(root.setup, root.tick) : []
            Item {
              required property var modelData
              width: panel.width
              implicitHeight: barName.implicitHeight
              Text {
                textFormat: Text.PlainText
                id: barName
                width: Style.space(120)
                text: modelData.label
                color: modelData.hot ? root.accent : root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.bodySmall
                font.bold: modelData.hot
              }
              Rectangle {
                id: track
                anchors.left: barName.right
                anchors.right: barValue.left
                anchors.leftMargin: Style.spacing.lg
                anchors.rightMargin: Style.spacing.lg
                anchors.verticalCenter: barName.verticalCenter
                height: Style.space(8)
                color: Util.alpha(root.foreground, 0.04)
                Rectangle {
                  property real target: Math.min(1, modelData.rpm / 6000)
                  width: parent.width * target
                  Behavior on width { enabled: root.live && !root.reducedMotion; NumberAnimation { duration: root.tickMs; easing.type: Easing.Linear } }
                  height: parent.height
                  color: modelData.hot ? root.accent : root.foreground
                  opacity: modelData.hot ? 1 : 0.55
                }
              }
              LiveNumber {
                id: barValue
                anchors.right: parent.right
                width: Style.space(84)
                horizontalAlignment: Text.AlignRight
                value: modelData.rpm
                step: 10
                grouped: true
                suffix: " RPM"
                tickMs: root.tickMs
                live: root.live
                reducedMotion: root.reducedMotion
                color: modelData.hot ? root.accent : root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.bodySmall
              }
            }
          }
          Text { textFormat: Text.PlainText; text: "0 to 6,000 RPM"; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.caption }
          PanelSectionHeader { text: "FOUND"; foreground: root.foreground; fontFamily: root.fontFamily }
          Flow {
            width: parent.width
            spacing: Style.spacing.sm
            Repeater {
              model: root.step === "pairing" ? Model.pairFound(root.setup) : []
              Rectangle {
                required property var modelData
                width: chip.implicitWidth + Style.spacing.md * 2
                height: chip.implicitHeight + Style.spacing.xs * 2
                color: Util.alpha(root.foreground, 0.04)
                border.width: 1
                border.color: Util.alpha(root.foreground, 0.4)
                Text { textFormat: Text.PlainText; id: chip; anchors.centerIn: parent; text: modelData; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.caption }
              }
            }
          }
        }

        // Calibration: each fan's table on its own scale, as it is measured (call 6).
        Grid {
          visible: root.step === "calibration"
          width: parent.width
          columns: 3
          columnSpacing: Style.spacing.lg
          rowSpacing: Style.spacing.lg
          Repeater {
            model: root.step === "calibration" ? Model.calCells(root.setup, root.tick) : []
            Column {
              required property var modelData
              width: (panel.width - Style.spacing.lg * 2) / 3
              spacing: Style.spacing.xs
              Item {
                width: parent.width
                implicitHeight: calName.implicitHeight
                Text { textFormat: Text.PlainText; id: calName; text: modelData.name; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.caption; font.bold: true }
                Text {
                  textFormat: Text.PlainText
                  anchors.right: parent.right
                  text: modelData.rpm === null ? "" : Model.formatNumber(Number(modelData.rpm), 10, 0, true)
                  color: root.foreground
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.caption
                  font.features: ({ "tnum": 1 })
                }
              }
              Text { textFormat: Text.PlainText; width: parent.width; elide: Text.ElideRight; text: modelData.status; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.caption }
              CalPlot {
                width: parent.width
                height: Style.space(84)
                table: modelData.table
                maxRpm: modelData.max
                duty: modelData.duty === null ? NaN : modelData.duty
                rpm: modelData.rpm === null ? NaN : Number(modelData.rpm)
                lowest: modelData.lowest === null || modelData.lowest === undefined ? NaN : Number(modelData.lowest)
                foreground: root.foreground
                accent: root.accent
                fontFamily: root.fontFamily
              }
            }
          }
        }

        // Load check (§5.5): the CPU under full load on the proposed Balanced curves, until it levels.
        Column {
          visible: root.step === "load-check" && root.load !== null
          width: parent.width
          spacing: Style.spacing.lg
          Item {
            width: parent.width
            implicitHeight: loadTitle.implicitHeight
            Text { textFormat: Text.PlainText; id: loadTitle; text: root.load ? root.load.title : ""; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.title; font.bold: true }
            Text {
              textFormat: Text.PlainText
              anchors.right: parent.right
              anchors.baseline: loadTitle.baseline
              text: root.load ? root.load.time : ""
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
              font.features: ({ "tnum": 1 })
            }
          }
          Text {
            textFormat: Text.PlainText
            width: parent.width
            wrapMode: Text.Wrap
            text: !root.load ? "" : root.load.loading
                ? "Every CPU thread is busy at the lowest priority, with the proposed Balanced curves running the fans. Passing "
                  + Math.round(root.load.ends) + " °C ends the pass: the curve's top moves 5 °C earlier, and the check runs again."
                : "The CPU passed " + Math.round(root.load.ends) + " °C, so the curve's top moved 5 °C earlier. The second pass starts once "
                  + "the CPU is back within 10 °C of idle, or after 2 minutes."
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
          }
          Row {
            spacing: Style.space(40)
            Column {
              spacing: Style.spacing.xs
              Text { textFormat: Text.PlainText; text: "CPU"; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.caption; font.bold: true }
              LiveNumber {
                value: root.load && root.load.celsius !== null ? root.load.celsius : NaN
                step: 0.1
                decimals: 1
                suffix: " °C"
                tickMs: root.tickMs
                live: root.live
                reducedMotion: root.reducedMotion
                color: root.load && root.load.celsius !== null && root.load.celsius >= root.load.ends ? root.urgent : root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.heading
              }
            }
            Column {
              spacing: Style.spacing.xs
              Text { textFormat: Text.PlainText; text: "CPU FANS"; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.caption; font.bold: true }
              LiveNumber {
                value: root.load && root.load.fans !== null ? root.load.fans : NaN
                step: 1
                suffix: " %"
                tickMs: root.tickMs
                live: root.live
                reducedMotion: root.reducedMotion
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.heading
              }
            }
          }
          // The last four minutes of the CPU, from 20 °C to its limit, with the line where a pass ends.
          Item {
            width: parent.width
            height: Style.space(120)
            Rectangle { anchors.fill: parent; color: Util.alpha(root.foreground, 0.04) }
            Sparkline {
              id: loadChart
              anchors.fill: parent
              lo: 20
              hi: root.load ? root.load.limit : 100
              window: 240
              tickMs: root.tickMs
              live: root.live
              reducedMotion: root.reducedMotion
              foreground: root.foreground
            }
            Rectangle {
              visible: root.load !== null
              width: parent.width
              height: 1
              y: root.load ? Math.round(loadChart.yOf(root.load.ends)) : 0
              color: Util.alpha(root.urgent, 0.8)
            }
            Text {
              textFormat: Text.PlainText
              visible: root.load !== null
              anchors.left: parent.left
              anchors.leftMargin: Style.spacing.sm
              y: root.load ? Math.round(loadChart.yOf(root.load.ends)) + Style.spacing.xs : 0
              text: root.load ? "a pass ends at " + Math.round(root.load.ends) + " °C" : ""
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
            }
          }
          Text { textFormat: Text.PlainText; text: "4 minutes · 20 to " + (root.load ? Math.round(root.load.limit) : 100) + " °C, the CPU's limit"; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.caption }
        }

        // Review: roles with their reasons, a name, Identify (call 7).
        Column {
          visible: root.step === "review"
          width: parent.width
          spacing: Style.spacing.md
          Text { textFormat: Text.PlainText; text: "Confirm what each fan is"; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.title; font.bold: true }
          Text {
            textFormat: Text.PlainText
            width: parent.width
            wrapMode: Text.Wrap
            text: "Press a role to change it; a dashed one needs your yes. Name a fan for what it cools; the header stays underneath."
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
          }
          Repeater {
            model: root.step === "review" ? root.review : []
            Column {
              required property var modelData
              width: panel.width
              spacing: Style.spacing.xs
              Row {
                spacing: Style.spacing.md
                TextField {
                  width: Style.space(170)
                  text: modelData.name
                  enabled: root.canWrite
                  foreground: root.foreground
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.bodySmall
                  onEditingFinished: if (text.trim() !== "" && text !== modelData.name) root.renameRequested(modelData.key, text.trim())
                }
                Button {
                  readonly property string role: root.roleOf(modelData)
                  readonly property bool waiting: modelData.confirm && root.roleChoice[modelData.key] === undefined
                  text: (Model.ROLE_NAME[role] || role) + (waiting ? "?" : "")
                  bordered: true
                  // The role that still needs a yes is the one that stands out.
                  active: waiting
                  enabled: root.canWrite && !modelData.nvml
                  foreground: root.foreground
                  accent: root.accent
                  fontFamily: root.fontFamily
                  fontSize: Style.font.caption
                  onClicked: root.chooseNext(modelData)
                }
                Button {
                  text: "Identify"
                  bordered: true
                  enabled: root.canWrite
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  fontSize: Style.font.caption
                  onClicked: root.identifyRequested(modelData.key)
                }
              }
              Text {
                textFormat: Text.PlainText
                width: parent.width
                wrapMode: Text.Wrap
                text: modelData.reason
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }
            }
          }
          PanelSectionHeader {
            visible: proposalText.text !== ""
            text: "CURVES"
            foreground: root.foreground
            fontFamily: root.fontFamily
          }
          Text {
            textFormat: Text.PlainText
            id: proposalText
            visible: text !== ""
            width: parent.width
            wrapMode: Text.Wrap
            text: Model.proposalText(root.setup, root.tick)
            color: root.setup && root.setup.proposed && root.setup.proposed.refused ? root.urgent : root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
          }
          Repeater {
            model: root.step === "review" && root.setup && root.setup.proposed && !root.setup.proposed.refused ? (root.setup.proposed.notes || []) : []
            Text {
              textFormat: Text.PlainText
              required property var modelData
              width: panel.width
              wrapMode: Text.Wrap
              text: modelData
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
            }
          }
        }

        // Stopped.
        Column {
          visible: root.step === "stopped"
          width: parent.width
          spacing: Style.spacing.lg
          topPadding: Style.space(40)
          Text { textFormat: Text.PlainText; text: "Stopped"; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.title; font.bold: true }
          Text {
            textFormat: Text.PlainText
            width: parent.width
            wrapMode: Text.Wrap
            text: root.setup && root.setup.stopped ? String(root.setup.stopped) : ""
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
          }
          Text {
            textFormat: Text.PlainText
            width: parent.width
            wrapMode: Text.Wrap
            text: "Every fan is back on its BIOS or driver, as it was found. Nothing was saved."
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
          }
        }
      }
    }

    Text {
      textFormat: Text.PlainText
      width: parent.width
      // Wrapped: at the review it carries the load check's result.
      wrapMode: Text.Wrap
      text: root.step === "start" ? "Nothing changes until you start. Every fan stays with its BIOS or driver."
          : root.setup ? String(root.setup.waiting || root.setup.say || "") : ""
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
    }
  }

  // ---------- the guards ----------
  Column {
    id: guardsCol
    anchors.right: parent.right
    width: Style.space(250)
    spacing: Style.spacing.lg
    PanelSectionHeader { text: "GUARDS"; foreground: root.foreground; fontFamily: root.fontFamily }
    Repeater {
      model: root.setup && Array.isArray(root.setup.guards) ? root.setup.guards : []
      Column {
        id: guard
        required property var modelData
        readonly property real c: modelData.celsius === null || modelData.celsius === undefined ? NaN : Number(modelData.celsius)
        readonly property bool tripped: !!root.setup && !!root.setup.stopped && !isNaN(c) && c >= Number(modelData.limit)
        width: guardsCol.width
        spacing: Style.spacing.xs
        Item {
          width: parent.width
          implicitHeight: gName.implicitHeight
          Text { textFormat: Text.PlainText; id: gName; text: guard.modelData.label; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
          Text {
            textFormat: Text.PlainText
            anchors.right: parent.right
            text: (isNaN(guard.c) ? "—" : guard.c.toFixed(1)) + " of " + Math.round(Number(guard.modelData.limit)) + " °C"
            color: guard.tripped ? root.urgent : root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
            font.features: ({ "tnum": 1 })
          }
        }
        Rectangle {
          width: parent.width
          height: Style.space(6)
          color: Util.alpha(root.foreground, 0.04)
          Rectangle {
            // From 20 °C, where every sparkline starts (§11.1 call 5), to the guard.
            width: parent.width * Math.max(0, Math.min(1, (guard.c - 20) / Math.max(1, Number(guard.modelData.limit) - 20)))
            Behavior on width { enabled: root.live && !root.reducedMotion; NumberAnimation { duration: root.tickMs; easing.type: Easing.Linear } }
            height: parent.height
            color: guard.tripped ? root.urgent : root.foreground
          }
        }
      }
    }
    Text {
      textFormat: Text.PlainText
      width: guardsCol.width
      wrapMode: Text.Wrap
      text: "If a guard trips, setup stops and every fan goes back to its BIOS or driver, as it was found. The emergency rule stays in force throughout."
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
    }
  }
}
