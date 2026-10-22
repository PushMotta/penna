import QtQuick
import qs.Commons
import qs.Ui

// The popup's profile pills (DESIGN.md §8.2, design review 03 call 1): Defaults first, set apart from the profiles by a
// wider gap, because it hands every fan back where the others choose curves. Otherwise the shell's ButtonGroup: one pick of
// N, driven by the popup's cursor through `cursorIndex`.
Row {
  id: root

  property var options: []          // Model.profileOptions: [{ value, label, builtin }]
  property string value: ""
  property int cursorIndex: -1
  property color foreground: Color.foreground
  property string fontFamily: Style.font.family

  signal changed(string value)
  signal hovered(int index, bool isHovered)

  spacing: Style.spacing.md

  Repeater {
    model: root.options

    delegate: Item {
      required property var modelData
      required property int index
      // The gap after Defaults is part of its slot, so the cursor indexes stay the options' own.
      readonly property bool apart: modelData.builtin === true && index < root.options.length - 1
      implicitWidth: pill.implicitWidth + (apart ? Style.spacing.xl : 0)
      implicitHeight: pill.implicitHeight

      Button {
        id: pill
        text: String(modelData.label)
        selected: String(modelData.value) === root.value
        hasCursor: root.cursorIndex === index
        bordered: true
        foreground: root.foreground
        fontFamily: root.fontFamily
        onClicked: root.changed(String(modelData.value))
        onHovered: function(h) { root.hovered(index, h) }
      }
    }
  }
}
