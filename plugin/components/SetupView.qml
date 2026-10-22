import QtQuick
import qs.Commons
import qs.Ui
import "../Model.js" as Model

// Setup in the studio (DESIGN.md §5, §8.3 as approved in design review 03, call 4). It takes the studio over: the fans on the
// left with their live RPM and step, the rail of steps and one panel per step in the middle, the guards on the right. It ends
// on the review: each fan's role with its reason, a name for it, and Identify. The action bar stays the studio's.
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

  // The review's choices, kept here until the user confirms them: control → role.
  property var roleChoice: ({})

  signal identifyRequested(string key)
  signal renameRequested(string key, string label)

  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property string step: starting || !setup ? "start" : setup.stopped ? "stopped" : String(setup.step)
  readonly property var rows: Model.setupRows(setup, tick)
  readonly property var review: Model.reviewRows(setup)

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

  implicitHeight: Math.max(left.implicitHeight, centre.implicitHeight, right.implicitHeight)

  // ---------- the fans ----------
  Column {
    id: left
    width: Style.space(250)
    spacing: Style.spacing.md
    PanelSectionHeader { text: "FANS"; foreground: root.foreground; fontFamily: root.fontFamily }
    Repeater {
      model: root.rows
      Rectangle {
        required property var modelData
        width: left.width
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
              text: modelData.name + (modelData.role ? " <font color=\"" + root.dim + "\"><small>" + modelData.role + "</small></font>" : "")
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
    anchors.left: left.right
    anchors.right: right.left
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
          Text { text: "Set up fans"; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.title; font.bold: true }
          Text {
            width: parent.width
            wrapMode: Text.Wrap
            text: "Penna measures every fan: what it reads back, which header it turns, its role, and its speed at every duty. "
                + "Nothing about your curves changes."
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
          }
          Repeater {
            model: [["1", "Idle sample", "30 s", "Watch every sensor before any fan is taken."],
                    ["2", "Write test", "5 s", "Take each fan, write 60 % and 80 %, read both back."],
                    ["3", "Pairing", "12 s a fan", "Find which header each control turns, one at a time."],
                    ["4", "Roles", "", "Suggest what each fan does, with the reason."],
                    ["5", "Calibration", "about 1:30", "Measure each fan's speed at every duty, all at once."],
                    ["6", "Review", "", "Confirm the roles and name the fans."]]
            Row {
              required property var modelData
              spacing: Style.spacing.lg
              Text { width: Style.space(18); text: modelData[0]; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
              Text { width: Style.space(110); text: modelData[1]; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall; font.bold: true }
              Text { width: Style.space(84); text: modelData[2]; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
              Text { text: modelData[3]; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
            }
          }
          Text {
            width: parent.width
            wrapMode: Text.Wrap
            text: "About four minutes, and you will hear the fans. The machine should be idle: setup waits while it is busy."
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
            Text { id: sName; text: Model.shortName(modelData) + " · " + modelData.label; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
            Text {
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
            Text { id: wName; text: modelData.name; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
            Text { anchors.right: parent.right; text: modelData.status; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
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
          Text { text: "0 to 6,000 RPM"; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.caption }
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
                Text { id: chip; anchors.centerIn: parent; text: modelData; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.caption }
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
                Text { id: calName; text: modelData.name; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.caption; font.bold: true }
                Text {
                  anchors.right: parent.right
                  text: modelData.rpm === null ? "" : Model.formatNumber(Number(modelData.rpm), 10, 0, true)
                  color: root.foreground
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.caption
                  font.features: ({ "tnum": 1 })
                }
              }
              Text { width: parent.width; elide: Text.ElideRight; text: modelData.status; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.caption }
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

        // Review: roles with their reasons, a name, Identify (call 7).
        Column {
          visible: root.step === "review"
          width: parent.width
          spacing: Style.spacing.md
          Text { text: "Confirm what each fan is"; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.title; font.bold: true }
          Text {
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
                  selected: !waiting
                  enabled: root.canWrite && !modelData.nvml
                  foreground: waiting ? root.accent : root.foreground
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
                width: parent.width
                wrapMode: Text.Wrap
                text: modelData.reason
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }
            }
          }
        }

        // Stopped.
        Column {
          visible: root.step === "stopped"
          width: parent.width
          spacing: Style.spacing.lg
          topPadding: Style.space(40)
          Text { text: "Stopped"; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.title; font.bold: true }
          Text {
            width: parent.width
            wrapMode: Text.Wrap
            text: root.setup && root.setup.stopped ? String(root.setup.stopped) : ""
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
          }
          Text {
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
      width: parent.width
      elide: Text.ElideRight
      text: root.step === "start" ? "Nothing changes until you start. Every fan stays with its BIOS or driver."
          : root.setup ? String(root.setup.waiting || root.setup.say || "") : ""
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
    }
  }

  // ---------- the guards ----------
  Column {
    id: right
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
        width: right.width
        spacing: Style.spacing.xs
        Item {
          width: parent.width
          implicitHeight: gName.implicitHeight
          Text { id: gName; text: guard.modelData.label; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
          Text {
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
            width: parent.width * Math.max(0, Math.min(1, (guard.c - 30) / Math.max(1, Number(guard.modelData.limit) - 30)))
            Behavior on width { enabled: root.live && !root.reducedMotion; NumberAnimation { duration: root.tickMs; easing.type: Easing.Linear } }
            height: parent.height
            color: guard.tripped ? root.urgent : root.foreground
          }
        }
      }
    }
    Text {
      width: right.width
      wrapMode: Text.Wrap
      text: "If a guard trips, setup stops and every fan goes back to its BIOS or driver, as it was found. The emergency rule stays in force throughout."
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
    }
  }
}
