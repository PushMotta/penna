// Pure logic shared by the QML surfaces and the node tests. No Qt here.
//
// Vocabulary (DESIGN.md §7.3):
//   tick      the body of a daemon `tick` event
//   control   one entry of tick.controls
//   sensor    one entry of tick.sensors

// Material Design glyphs from the shell's Nerd Font (DESIGN.md §8.1), as surrogate-pair literals: in the shell, built-ins
// such as String are not reachable while a plugin script initialises, so nothing at top level may call one.
var GLYPH = {
  fan: "󰈐",    // U+F0210 fan
  full: "󱑮",   // U+F146E fan-chevron-up
  timed: "󱨺",  // U+F1A3A fan-clock
  alert: "󱑬",  // U+F146C fan-alert
  off: "󰠝",    // U+F081D fan-off
  auto: "󱜝"    // U+F171D fan-auto
}

var ALERT_STATES = ["failsafe", "emergency", "contested", "fault", "stalled"]

var STATE_META = {
  failsafe: "A SENSOR FAILED · AT ITS FAILSAFE SPEED",
  emergency: "AT 100 % UNTIL THE EMERGENCY ENDS",
  contested: "SOMETHING ELSE KEEPS SETTING IT",
  fault: "A WRITE FAILED · HANDED BACK",
  stalled: "ITS FAN ISN'T TURNING"
}

function socketCandidates(pennaSocket, runtimeDir) {
  var list = []
  if (pennaSocket) list.push(String(pennaSocket))
  list.push("/run/penna/penna.sock")
  if (runtimeDir) list.push(String(runtimeDir).replace(/\/+$/, "") + "/penna/penna.sock")
  return list.filter(function(p, i) { return list.indexOf(p) === i })
}

function controls(tick) { return tick && Array.isArray(tick.controls) ? tick.controls : [] }

// The fans Penna runs: configured controls a profile adopts and that are present. A configured control on "firmware" folds
// under Firmware and an absent one under Not reachable (§8.2); neither is a fan Penna runs.
function adopted(tick) { return controls(tick).filter(function(c) { return c.state !== "firmware" && c.state !== "absent" }) }

function firmwareLines(tick) {
  var listed = tick && Array.isArray(tick.firmware) ? tick.firmware.map(function(f) { return String(f.label) }) : []
  return controls(tick).filter(function(c) { return c.state === "firmware" }).map(function(c) { return String(c.label) }).concat(listed)
}

function unreachableLines(tick) {
  var absent = controls(tick).filter(function(c) { return c.state === "absent" })
                             .map(function(c) { return String(c.label) + " · configured, but not on this machine now" })
  var list = tick && Array.isArray(tick.unreachable) ? tick.unreachable : []
  return absent.concat(list.map(function(u) { return String(u.label) + " · " + String(u.reason) }))
}
function sensors(tick) { return tick && Array.isArray(tick.sensors) ? tick.sensors : [] }
function isAlert(state) { return ALERT_STATES.indexOf(state) >= 0 }
function alerts(tick) { return controls(tick).filter(function(c) { return isAlert(c.state) }) }
function hasEmergency(tick) { return !!(tick && Array.isArray(tick.emergency) && tick.emergency.length) }

function fanCount(tick) {
  return adopted(tick).reduce(function(n, c) { return n + Math.max(1, Number(c.fans) || 0) }, 0)
}

// The bar glyph and its colour role: shape first, colour only reinforces (§8.1, review call 1).
function barState(tick, connected) {
  if (!connected) return { glyph: GLYPH.off, tone: "dim" }
  if (!tick) return { glyph: GLYPH.fan, tone: "dim" }
  if (hasEmergency(tick) || alerts(tick).length) return { glyph: GLYPH.alert, tone: "urgent" }
  if (tick.full_speed) return { glyph: GLYPH.full, tone: "accent" }
  var held = controls(tick).filter(function(c) { return !!c.override })
  if (held.length) {
    var timed = held.some(function(c) { return c.override.until !== null && c.override.until !== undefined })
    return { glyph: timed ? GLYPH.timed : GLYPH.full, tone: "accent" }
  }
  if (!adopted(tick).length) return { glyph: GLYPH.auto, tone: "normal" }
  return { glyph: GLYPH.fan, tone: "normal" }
}

function profileLabel(tick) {
  if (!tick) return "Penna"
  var list = Array.isArray(tick.profiles) ? tick.profiles : []
  for (var i = 0; i < list.length; i++) if (list[i].id === tick.profile) return String(list[i].label || list[i].id)
  return tick.profile ? capitalise(String(tick.profile)) : "Penna"
}

function profileOptions(tick) {
  var list = tick && Array.isArray(tick.profiles) ? tick.profiles : []
  return list.map(function(p) { return { value: String(p.id), label: String(p.label || capitalise(String(p.id))) } })
}

function capitalise(s) { return s.length ? s.charAt(0).toUpperCase() + s.slice(1) : s }

// Repeaters are keyed by these lists, which change only when the set of rows does. A Repeater given a fresh array on
// every tick rebuilds every delegate (cards, sparklines, numbers), which costs a dropped frame and resets every glide.
function keysOf(list) { return (list || []).map(function(x) { return String(x.key) }) }

function sameKeys(a, b) {
  if (!a || !b || a.length !== b.length) return false
  for (var i = 0; i < a.length; i++) if (a[i] !== b[i]) return false
  return true
}

function byKey(list, key) {
  if (!list) return null
  for (var i = 0; i < list.length; i++) if (list[i].key === key) return list[i]
  return null
}

// The popup's hero: one line that states the situation, or names what is wrong (§8.2). `meta` is null in the normal
// case; the popup builds it from the temperatures it is showing, so the hero and the rows never disagree.
function hero(tick, connected, nowSec, lastTickSec) {
  if (!connected) {
    return { title: "Penna isn't running", tone: "dim", glyph: GLYPH.off,
             meta: lastTickSec > 0 ? "LAST UPDATE " + ago(nowSec - lastTickSec) : "NOT CONNECTED" }
  }
  if (!tick) return { title: "Penna", tone: "dim", glyph: GLYPH.fan, meta: "WAITING FOR THE FIRST TICK" }
  if (hasEmergency(tick)) {
    var e = tick.emergency[0]
    return { title: String(e.label) + " at " + Math.round(Number(e.celsius)) + " °C", tone: "urgent", glyph: GLYPH.alert,
             meta: "EVERY FAN AT 100 % UNTIL " + Math.round(Number(e.release)) + " °C" }
  }
  var a = alerts(tick)
  if (a.length === 1) return { title: a[0].label + " is " + a[0].state, tone: "urgent", glyph: GLYPH.alert, meta: STATE_META[a[0].state] || "" }
  if (a.length > 1) return { title: a.length + " fans need attention", tone: "urgent", glyph: GLYPH.alert, meta: "EACH ONE SAYS WHY BELOW" }
  if (tick.full_speed) return { title: "Full speed", tone: "accent", glyph: GLYPH.full, meta: "EVERY FAN AT 100 % · UNTIL YOU TURN IT OFF" }
  if (tick.draft) return { title: profileLabel(tick), tone: "accent", glyph: GLYPH.fan, meta: "PREVIEWING UNSAVED CHANGES FROM THE STUDIO" }
  if (tick.profile === "defaults") return { title: "Defaults", tone: "normal", glyph: GLYPH.auto, meta: "THE BIOS AND DRIVERS RUN EVERY FAN" }
  if (!adopted(tick).length) return { title: "Firmware runs every fan", tone: "normal", glyph: GLYPH.auto, meta: "NOTHING ADOPTED YET" }
  return { title: profileLabel(tick), tone: "normal", glyph: GLYPH.fan, meta: null }
}

function normalMeta(fans, temps) {
  var parts = [fans + (fans === 1 ? " FAN" : " FANS")]
  for (var i = 0; i < temps.length && i < 2; i++) parts.push(temps[i].name.toUpperCase() + " " + temps[i].text)
  return parts.join(" · ")
}

function shortName(sensor) {
  if (!sensor) return ""
  if (sensor.virtual) return String(sensor.label)
  var d = String(sensor.device || "")
  if (/cpu|k10temp|coretemp|ryzen|intel/i.test(d)) return "CPU"
  if (/gpu|nvidia|geforce|radeon|amdgpu/i.test(d)) return "GPU"
  if (/nvme/i.test(d)) return "NVMe"
  return d || String(sensor.label)
}

// One row per source a curve reads (§8.2); with nothing adopted, the plausible sensors instead, up to four.
function tempRows(tick) {
  var all = sensors(tick)
  var byKey = {}
  all.forEach(function(s) { byKey[s.key] = s })
  var keys = []
  var curves = tick && Array.isArray(tick.curves) ? tick.curves : []
  curves.forEach(function(c) { if (c.source && byKey[c.source] && keys.indexOf(c.source) < 0) keys.push(c.source) })
  if (!keys.length) {
    keys = all.filter(function(s) { return !s.below_ambient && s.celsius !== null && s.celsius !== undefined && !s.virtual })
              .slice(0, 4).map(function(s) { return s.key })
  }
  return keys.map(function(k) {
    var s = byKey[k]
    var name = shortName(s)
    var label = String(s.label)
    return { key: k, name: name, detail: s.virtual || label.toLowerCase() === name.toLowerCase() ? "" : label,
             celsius: s.celsius, error: s.error || "", lo: scale(s)[0], hi: scale(s)[1] }
  })
}

function scale(sensor) {
  return sensor && Array.isArray(sensor.scale) && sensor.scale.length === 2 ? sensor.scale : [20, 100]
}

// The badge a fan row carries, if any (review section F).
function badge(control, tick, nowSec) {
  if (!control) return null
  if (isAlert(control.state)) return { text: control.state.toUpperCase(), tone: "urgent" }
  if (control.state === "handback") return { text: "DRIVER", tone: "dim" }
  if (control.state === "override" || control.override) {
    if (tick && tick.full_speed) return { text: "FULL", tone: "accent" }
    var until = control.override ? control.override.until : null
    if (until !== null && until !== undefined) return { text: clock(Number(until) - nowSec), tone: "accent" }
    return { text: "HELD", tone: "accent" }
  }
  return null
}

function dutyText(control) {
  if (!control || control.state === "handback" || control.duty === null || control.duty === undefined) return "—"
  return Math.round(Number(control.duty)) + " %"
}

// The shown number changes only once the value has moved 0.6 of a step past it, so a value sitting on a rounding
// boundary does not flicker (§8.6, review call 8).
function deadband(shown, value, step) {
  if (value === null || value === undefined || isNaN(value)) return shown
  var s = step > 0 ? step : 1
  if (shown === null || shown === undefined || isNaN(shown)) return Math.round(value / s) * s
  if (Math.abs(value - shown) >= 0.6 * s) return Math.round(value / s) * s
  return shown
}

function formatNumber(value, step, decimals, grouped) {
  if (value === null || value === undefined || isNaN(value)) return "—"
  var s = step > 0 ? step : 1
  var v = Math.round(value / s) * s
  var text = v.toFixed(decimals > 0 ? decimals : 0)
  if (grouped) text = text.replace(/\B(?=(\d{3})+(?!\d))/g, ",")
  return text
}

function ago(seconds) {
  var s = Math.max(0, Math.round(seconds))
  if (s < 5) return "JUST NOW"
  if (s < 90) return s + " S AGO"
  if (s < 5400) return Math.round(s / 60) + " MIN AGO"
  return Math.round(s / 3600) + " H AGO"
}

function clock(seconds) {
  var s = Math.max(0, Math.ceil(seconds))
  var m = Math.floor(s / 60)
  var r = s % 60
  return m + ":" + (r < 10 ? "0" : "") + r
}

// ---------------------------------------------------------------- studio (M3)

var T_MIN = 20
var T_MAX = 100

function clamp(v, lo, hi) { return Math.max(lo, Math.min(hi, v)) }

function copyPoints(points) { return (points || []).map(function(p) { return [Number(p[0]), Number(p[1])] }) }

function pointsEqual(a, b) {
  if (!a || !b || a.length !== b.length) return false
  for (var i = 0; i < a.length; i++) if (a[i][0] !== b[i][0] || a[i][1] !== b[i][1]) return false
  return true
}

function evalPoints(points, t) {
  if (!points || !points.length) return 0
  if (t <= points[0][0]) return points[0][1]
  for (var i = 1; i < points.length; i++) {
    if (t <= points[i][0]) {
      var a = points[i - 1]
      var b = points[i]
      return a[1] + (b[1] - a[1]) * (t - a[0]) / (b[0] - a[0])
    }
  }
  return points[points.length - 1][1]
}

// Moves point i to whole °C and %, between its neighbours, and pushes the others so the curve never dips (decision 3).
function movePoint(points, i, t, d) {
  var next = copyPoints(points)
  if (i < 0 || i >= next.length) return next
  var lo = i === 0 ? T_MIN : next[i - 1][0] + 1
  var hi = i === next.length - 1 ? T_MAX : next[i + 1][0] - 1
  var nt = clamp(Math.round(t), lo, hi)
  var nd = clamp(Math.round(d), 0, 100)
  next[i] = [nt, nd]
  for (var j = i + 1; j < next.length; j++) next[j][1] = Math.max(next[j][1], nd)
  for (var k = i - 1; k >= 0; k--) next[k][1] = Math.min(next[k][1], nd)
  return next
}

// A point halfway to the next one, if there is a whole degree of room; else nothing changes. At most eight points.
function addPoint(points, i) {
  var next = copyPoints(points)
  if (next.length >= 8 || i < 0 || i >= next.length - 1) return { points: next, index: i }
  var a = next[i]
  var b = next[i + 1]
  if (b[0] - a[0] < 2) return { points: next, index: i }
  next.splice(i + 1, 0, [Math.round((a[0] + b[0]) / 2), Math.round((a[1] + b[1]) / 2)])
  return { points: next, index: i + 1 }
}

function deletePoint(points, i) {
  var next = copyPoints(points)
  if (next.length <= 2 || i < 0 || i >= next.length) return { points: next, index: i }
  next.splice(i, 1)
  return { points: next, index: Math.min(i, next.length - 1) }
}

// The hysteresis band: where a falling output may be held, between f(T) and f(T + H) (§4.4). [t, d] pairs, closed.
function bandPolygon(points, hysteresis, step) {
  var h = Number(hysteresis) || 0
  if (h <= 0 || !points || points.length < 2) return []
  var s = step > 0 ? step : 1
  var top = []
  var bottom = []
  for (var t = T_MIN; t <= T_MAX + 1e-9; t += s) {
    top.push([t, evalPoints(points, Math.min(T_MAX, t + h))])
    bottom.push([t, evalPoints(points, t)])
  }
  return top.concat(bottom.reverse())
}

// RPM for a duty from the measured table (§5.2), or null before calibration.
function rpmAt(calibration, duty) {
  if (!calibration || !calibration.length) return null
  var table = calibration
  if (duty < table[0][0]) return 0
  for (var i = 1; i < table.length; i++) {
    if (duty <= table[i][0]) {
      var a = table[i - 1]
      var b = table[i]
      return a[1] + (b[1] - a[1]) * (duty - a[0]) / Math.max(1e-9, b[0] - a[0])
    }
  }
  return table[table.length - 1][1]
}

function readout(t, d, control) {
  var head = Math.round(t) + " °C → " + Math.round(d) + " %"
  if (!control) return head
  if (control.nvml && d < Number(control.nvml.min)) return head + " driver curve"
  if (control.stop !== null && control.stop !== undefined && d < Number(control.stop) && control.may_stop) return head + " stopped"
  var r = rpmAt(control.calibration, d)
  return r === null ? head : head + " ≈ " + formatNumber(r, 10, 0, true) + " RPM"
}

// The inspector line under the editor: what the output is doing, in words (§8.3).
function inspector(curveLive, control, controlLive, tick) {
  if (!curveLive) return ""
  if (controlLive && controlLive.state === "override") return tick && tick.full_speed ? "Full speed holds this fan at 100 %" : "An override holds this fan"
  if (controlLive && controlLive.state === "handback") {
    var floor = control && control.nvml ? Number(control.nvml.min) : 30
    return "Below the " + floor + " % floor: the driver runs this fan (" + formatNumber(controlLive.rpm, 10, 0, true) + " RPM)"
  }
  if (controlLive && isAlert(controlLive.state)) return capitalise(controlLive.state) + (controlLive.reason ? ": " + controlLive.reason : "")
  if (curveLive.held && curveLive.hold_until !== null && curveLive.hold_until !== undefined) {
    return "Holding: hysteresis keeps it until " + Number(curveLive.hold_until).toFixed(1) + " °C"
  }
  if (controlLive && controlLive.trend === "rising") {
    return "Rising at the step limit, " + Math.round(Number(control ? control.step_up : 20)) + " %/s → " + Math.round(Number(controlLive.target)) + " %"
  }
  if (controlLive && controlLive.trend === "falling") {
    return "Falling at the step limit, " + Math.round(Number(control ? control.step_down : 5)) + " %/s → " + Math.round(Number(controlLive.target)) + " %"
  }
  return "Following the curve"
}

function limitsLine(control) {
  if (!control) return ""
  if (control.nvml) return "Driver below " + control.nvml.min + " %" + (control.may_stop ? ", which may stop it" : "") + " · steps ↑" + control.step_up + " ↓" + control.step_down + " %/s"
  var parts = ["min " + Math.round(control.floor) + " %"]
  if (control.stop !== null && control.stop !== undefined) parts.push((control.may_stop ? "stops below " : "stalls below ") + Math.round(control.stop) + " %")
  parts.push("steps ↑" + control.step_up + " ↓" + control.step_down + " %/s")
  return parts.join(" · ")
}

// One editor for three kinds (§4.3): a Graph's points, a Trigger's two corners [off, idle] and [on, load], a Flat's level.
function handlesOf(curve) {
  if (!curve) return []
  if (curve.kind === "graph") return copyPoints(curve.points)
  if (curve.kind === "trigger" && curve.trigger) {
    var g = curve.trigger
    return [[Number(g.off), Number(g.idle)], [Number(g.on), Number(g.load)]]
  }
  if (curve.kind === "flat") return [[60, Number(curve.duty)]]
  return []
}

function curveWith(curve, handles) {
  var next = JSON.parse(JSON.stringify(curve))
  if (curve.kind === "graph") next.points = copyPoints(handles)
  else if (curve.kind === "trigger") next.trigger = { idle: handles[0][1], load: handles[1][1], off: handles[0][0], on: handles[1][0] }
  else if (curve.kind === "flat") next.duty = handles[0][1]
  return next
}

function moveHandle(kind, handles, i, t, d) {
  if (kind === "graph") return movePoint(handles, i, t, d)
  var next = copyPoints(handles)
  if (i < 0 || i >= next.length) return next
  if (kind === "flat") {
    next[0] = [next[0][0], clamp(Math.round(d), 0, 100)]
    return next
  }
  // Trigger: off stays at least a degree below on, and idle never above load.
  if (i === 0) next[0] = [clamp(Math.round(t), T_MIN, next[1][0] - 1), clamp(Math.round(d), 0, next[1][1])]
  else next[1] = [clamp(Math.round(t), next[0][0] + 1, T_MAX), clamp(Math.round(d), next[0][1], 100)]
  return next
}

// The line the editor draws, as [t, d] pairs from 20 to 100 °C.
function pathOf(kind, handles) {
  if (!handles.length) return []
  if (kind === "graph") return [[T_MIN, handles[0][1]]].concat(copyPoints(handles)).concat([[T_MAX, handles[handles.length - 1][1]]])
  if (kind === "flat") return [[T_MIN, handles[0][1]], [T_MAX, handles[0][1]]]
  if (kind === "trigger") {
    var off = handles[0]
    var on = handles[1]
    return [[T_MIN, off[1]], [on[0], off[1]], [on[0], on[1]], [T_MAX, on[1]]]
  }
  return []
}

// The shaded region: a Graph's hysteresis band, or the gap where a Trigger keeps whichever state it is in.
function shadeOf(curve, handles) {
  if (!curve) return []
  if (curve.kind === "graph") return bandPolygon(handles, curve.hysteresis, 0.5)
  if (curve.kind === "trigger" && handles.length === 2) {
    var off = handles[0]
    var on = handles[1]
    return [[off[0], off[1]], [on[0], off[1]], [on[0], on[1]], [off[0], on[1]]]
  }
  return []
}

function curveOptions(body) {
  var list = body && Array.isArray(body.curves) ? body.curves : []
  return list.map(function(c) { return { value: String(c.id), label: String(c.label) } }).concat([{ value: "firmware", label: "Firmware" }])
}

function writeBlockedReason(connected, canWrite) {
  if (!connected || canWrite) return ""
  return "Changing fans needs the penna group: sudo usermod -aG penna $USER (it applies at once)."
}

if (typeof module !== "undefined") {
  module.exports = {
    GLYPH: GLYPH, ALERT_STATES: ALERT_STATES, socketCandidates: socketCandidates, controls: controls, sensors: sensors,
    isAlert: isAlert, alerts: alerts, adopted: adopted, firmwareLines: firmwareLines, unreachableLines: unreachableLines, hasEmergency: hasEmergency, fanCount: fanCount, barState: barState,
    profileLabel: profileLabel, profileOptions: profileOptions, hero: hero, normalMeta: normalMeta, shortName: shortName,
    tempRows: tempRows, scale: scale, badge: badge, dutyText: dutyText, deadband: deadband, formatNumber: formatNumber,
    ago: ago, clock: clock, writeBlockedReason: writeBlockedReason, keysOf: keysOf, sameKeys: sameKeys, byKey: byKey,
    T_MIN: T_MIN, T_MAX: T_MAX, clamp: clamp, copyPoints: copyPoints, pointsEqual: pointsEqual, evalPoints: evalPoints,
    movePoint: movePoint, addPoint: addPoint, deletePoint: deletePoint, bandPolygon: bandPolygon, rpmAt: rpmAt, readout: readout,
    inspector: inspector, limitsLine: limitsLine, curveOptions: curveOptions, handlesOf: handlesOf, curveWith: curveWith,
    moveHandle: moveHandle, pathOf: pathOf, shadeOf: shadeOf
  }
}
