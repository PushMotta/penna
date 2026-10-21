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
function sensors(tick) { return tick && Array.isArray(tick.sensors) ? tick.sensors : [] }
function isAlert(state) { return ALERT_STATES.indexOf(state) >= 0 }
function alerts(tick) { return controls(tick).filter(function(c) { return isAlert(c.state) }) }
function hasEmergency(tick) { return !!(tick && Array.isArray(tick.emergency) && tick.emergency.length) }

function fanCount(tick) {
  return controls(tick).reduce(function(n, c) { return n + Math.max(1, Number(c.fans) || 0) }, 0)
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
  if (!controls(tick).length) return { glyph: GLYPH.auto, tone: "normal" }
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
  if (!controls(tick).length) return { title: "Firmware runs every fan", tone: "normal", glyph: GLYPH.auto, meta: "NOTHING ADOPTED YET" }
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

function writeBlockedReason(connected, canWrite) {
  if (!connected || canWrite) return ""
  return "Changing fans needs the penna group: sudo usermod -aG penna $USER, then log in again."
}

if (typeof module !== "undefined") {
  module.exports = {
    GLYPH: GLYPH, ALERT_STATES: ALERT_STATES, socketCandidates: socketCandidates, controls: controls, sensors: sensors,
    isAlert: isAlert, alerts: alerts, hasEmergency: hasEmergency, fanCount: fanCount, barState: barState,
    profileLabel: profileLabel, profileOptions: profileOptions, hero: hero, normalMeta: normalMeta, shortName: shortName,
    tempRows: tempRows, scale: scale, badge: badge, dutyText: dutyText, deadband: deadband, formatNumber: formatNumber,
    ago: ago, clock: clock, writeBlockedReason: writeBlockedReason
  }
}
