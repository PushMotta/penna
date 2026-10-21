import QtQuick
import "../Model.js" as Model

// A number that never jumps or flickers (DESIGN.md §8.6): the value glides linearly over one tick, the shown figure
// changes only past a dead band of 0.6 of a step, and each change eases in over 140 ms. While `live` is false (the
// popup is closed) or under reduced motion, it snaps and costs nothing per frame.
Text {
  id: root

  property real value: NaN
  property real step: 1
  property int decimals: 0
  property bool grouped: false
  property string suffix: ""
  property string placeholder: "—"
  property int tickMs: 1000
  property bool live: true
  property bool reducedMotion: false

  readonly property bool animate: live && !reducedMotion && primed
  property bool primed: false
  property real glided: 0
  property real shown: NaN
  property real eased: 0

  Behavior on glided { enabled: root.animate; NumberAnimation { duration: root.tickMs; easing.type: Easing.Linear } }
  Behavior on eased { enabled: root.animate; NumberAnimation { duration: 140; easing.type: Easing.OutCubic } }

  function snap() {
    if (isNaN(value)) return
    primed = false
    glided = value
    shown = Model.deadband(NaN, value, step)
    eased = shown
    primed = true
  }

  onValueChanged: {
    if (isNaN(value)) return
    if (!primed || isNaN(shown)) snap()
    else glided = value
  }
  onLiveChanged: if (live) snap()
  onGlidedChanged: {
    var s = Model.deadband(shown, glided, step)
    if (s !== shown) {
      shown = s
      eased = s
    }
  }
  Component.onCompleted: snap()

  text: isNaN(value) ? placeholder : Model.formatNumber(eased, step, decimals, grouped) + suffix
  textFormat: Text.PlainText
  font.features: ({ "tnum": 1 })
}
