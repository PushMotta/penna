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

// $PENNA_SOCKET alone when set: a socket named on purpose is never swapped for this machine's daemon (§7.3).
function socketCandidates(pennaSocket, runtimeDir) {
  if (pennaSocket) return [String(pennaSocket)]
  var list = []
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
  // Setup holds every fan for a while and then lets go: the timed glyph (§11.3 call 4).
  if (setupRunning(tick)) return { glyph: GLYPH.timed, tone: "accent" }
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

// The pills (§11.3 calls 1 and 2): Defaults first, then each profile that runs a fan, and the active one whatever it runs. A
// daemon that doesn't say how many fans a profile runs shows every profile, as before.
function profileOptions(tick) {
  var list = tick && Array.isArray(tick.profiles) ? tick.profiles : []
  return list.filter(function(p) { return p.builtin || p.fans === undefined || Number(p.fans) > 0 || p.id === tick.profile })
             .map(function(p) { return { value: String(p.id), label: String(p.label || capitalise(String(p.id))), builtin: !!p.builtin } })
}

// A new install: only Defaults to choose from, so no pills, and Set up fans… is the one thing to press (§11.3 call 2).
function needsSetup(tick) {
  return !!tick && profileOptions(tick).filter(function(p) { return !p.builtin }).length === 0
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
  var s = tick.setup
  if (s && !s.stopped && s.step !== "review") return { title: "Setting up fans", tone: "accent", glyph: GLYPH.timed, meta: setupMeta(s) }
  if (s && s.stopped) return { title: "Setup stopped", tone: "normal", glyph: GLYPH.auto, meta: "EVERY FAN IS BACK AS IT WAS FOUND" }
  if (s && s.step === "review") return { title: "Setup is done", tone: "normal", glyph: GLYPH.auto, meta: "CONFIRM THE ROLES IN THE STUDIO" }
  if (tick.full_speed) return { title: "Full speed", tone: "accent", glyph: GLYPH.full, meta: "EVERY FAN AT 100 % · UNTIL YOU TURN IT OFF" }
  if (tick.draft) return { title: profileLabel(tick), tone: "accent", glyph: GLYPH.fan, meta: "PREVIEWING UNSAVED CHANGES FROM THE STUDIO" }
  if (tick.profile === "defaults") return { title: "Defaults", tone: "normal", glyph: GLYPH.auto, meta: "THE BIOS AND DRIVERS RUN EVERY FAN" }
  if (!adopted(tick).length) return { title: "Firmware runs every fan", tone: "normal", glyph: GLYPH.auto, meta: "NOTHING ADOPTED YET" }
  return { title: profileLabel(tick), tone: "normal", glyph: GLYPH.fan, meta: null }
}

// ---------------------------------------------------------------- setup and Defaults (design review 03, §11.3)

var STEP_NAME = { "idle": "Idle sample", "write-test": "Write test", "pairing": "Pairing", "roles": "Roles", "calibration": "Calibration", "load-check": "Load check", "review": "Review" }

function isNvmlKey(key) { return String(key).indexOf("nvml/") === 0 }

function setupRunning(tick) {
  var s = tick ? tick.setup : null
  return !!s && !s.stopped && s.step !== "review"
}

// The hero's line while setup runs: the step, how far, and roughly how long is left.
function setupMeta(s) {
  if (!s) return ""
  if (s.waiting) return "WAITING FOR THE MACHINE TO IDLE"
  var head = String(STEP_NAME[s.step] || s.step).toUpperCase()
  if (s.step === "pairing") {
    var hw = (s.controls || []).filter(function(c) { return !isNvmlKey(c.key) && c.write === "ok" })
    var done = hw.filter(function(c) { return c.paired !== null && c.paired !== undefined }).length
    head += " · " + Math.min(done + 1, hw.length) + " OF " + hw.length
  }
  return head + " · ABOUT " + clock(Number(s.left) || 0) + " LEFT"
}

// Pairing has run on this machine when any tach names the control that turns it.
function paired(tick) { return !!(tick && Array.isArray(tick.tachs) && tick.tachs.some(function(t) { return !!t.control })) }

// In Defaults every fan stays listed (§11.3 call 3): each motherboard fan by its header with its RPM, then each GPU fan, with
// who runs it in place of a curve. Never a duty: while the BIOS runs a fan, the IT8688E's PWM register reads the BIOS
// curve's start value. Before pairing the rows are tachs; after it, a tach no control turns is an empty header.
function defaultsRows(tick) {
  if (!tick) return []
  var after = paired(tick)
  var rows = (Array.isArray(tick.tachs) ? tick.tachs : []).filter(function(t) { return !after || !!t.control }).map(function(t) {
    return { key: String(t.key), label: String(t.label), fans: 1, rpm: t.rpm, state: "firmware", duty: null, curve_label: "BIOS" }
  })
  var gpus = controls(tick).filter(function(c) { return isNvmlKey(c.key) })
             .concat((Array.isArray(tick.firmware) ? tick.firmware : []).filter(function(f) { return isNvmlKey(f.key) }))
  gpus.forEach(function(c) {
    rows.push({ key: String(c.key), label: String(c.label), fans: 1, rpm: c.rpm === undefined ? null : c.rpm, state: "firmware",
                duty: null, curve_label: "Driver" })
  })
  return rows
}

function emptyHeaders(tick) {
  if (!paired(tick)) return []
  return tick.tachs.filter(function(t) { return !t.control }).map(function(t) { return String(t.label) + " · no fan turns on it" })
}

// The device a control belongs to, for the studio's card headings (§11.3 call 10): its chip or GPU, what kind of device it is,
// and how far its driver has been checked. The tier is said once here instead of on every card.
function deviceOf(control, tick) {
  var key = String(control.key)
  var parts = key.split("/")
  var id = parts.slice(0, 3).join("/")
  var name = ""
  var kind = ""
  if (parts[0] === "nvml") {
    id = parts.slice(0, 2).join("/")
    var s = sensors(tick).filter(function(x) { return String(x.key).indexOf(id + "/") === 0 })[0]
    name = s ? String(s.device).replace(/^NVIDIA (GeForce )?/, "") : "GPU"
    kind = "NVML"
  } else {
    var t = (tick && Array.isArray(tick.tachs) ? tick.tachs : []).filter(function(x) { return String(x.key).indexOf(id + "/") === 0 })[0]
    name = t ? String(t.device) : parts[1]
    kind = "motherboard"
  }
  return { id: id, name: name, kind: kind, tier: control.tier ? String(control.tier) : "" }
}

// A drifting fan's line on its card (§5.6): how much slower than when it was set up, and why that might be.
function driftLine(control) {
  if (!control || control.drift === null || control.drift === undefined) return ""
  return "Turns " + Math.round(Number(control.drift) * 100) + " % slower than when it was set up: dust, or a tired bearing."
}

function tierWord(tier) { return tier ? capitalise(String(tier)) : "" }

// One sentence under a heading whose tier is below Verified.
function tierNote(tier) {
  if (tier === "documented") return "Nobody has verified this driver with Penna yet."
  if (tier === "captured") return "Runs in Penna's tests from a real machine; nobody has verified it here."
  return ""
}

// The cards in device order, each group with its heading: [{ id, heading, tier, note, keys }].
function deviceGroups(list, tick) {
  var groups = []
  ;(list || []).forEach(function(c) {
    var d = deviceOf(c, tick)
    var g = groups.filter(function(x) { return x.id === d.id })[0]
    if (!g) {
      g = { id: d.id, heading: (d.name + " · " + d.kind).toUpperCase(), tier: tierWord(d.tier), note: tierNote(d.tier), keys: [] }
      groups.push(g)
    }
    g.keys.push(String(c.key))
  })
  return groups
}

// A card's kind line, only when the kind isn't plain duty or NVML (§11.3 call 10).
function kindLine(control) {
  if (!control || !control.kind || control.kind === "duty" || control.kind === "nvml") return ""
  var k = String(control.kind)
  if (k === "levels") return "Fixed levels. The curve is rounded to a level."
  if (k === "speed") return "A target speed in RPM."
  if (k === "firmware-curve") return "Its firmware runs the curve itself, so it keeps running if Penna stops."
  if (k === "modes") return "Automatic or full speed only; there is no curve."
  if (k === "read-only") return "Penna only reads this fan."
  return ""
}

// ---- the studio's setup view (§8.3 as approved in §11.3 call 4)

// The rail: every step, with its time on this machine, done, on, or to come.
function setupRail(s) {
  if (!s) return []
  var order = (s.steps || []).map(function(x) { return x.step })
  var at = order.indexOf(s.step)
  return (s.steps || []).map(function(x, i) {
    var secs = Number(x.seconds) || 0
    var time = secs <= 0 ? "" : secs < 60 ? " · " + Math.round(secs) + " s" : " · " + clock(secs)
    return { label: (i + 1) + " " + (STEP_NAME[x.step] || x.step) + time, state: s.step === "review" ? (x.step === "review" ? "on" : "done") : i < at ? "done" : i === at ? "on" : "todo" }
  })
}

// One row per fan in the setup view's left column: its header once paired, its live RPM and what is happening to it.
function setupRows(s, tick) {
  if (!s) return []
  return (s.controls || []).map(function(c) {
    var live = byKey(controls(tick), c.key)
    var tach = c.tachs && c.tachs.length ? byKey(tick && tick.tachs, c.tachs[0]) : null
    var rpm = tach ? tach.rpm : live ? live.rpm : null
    var role = c.suggestion ? String(c.suggestion.role) + (c.suggestion.confirm ? "?" : "") : ""
    return { key: String(c.key), name: String(c.header || c.label), status: String(c.status || ""), rpm: rpm === undefined ? null : rpm,
             duty: c.duty === null || c.duty === undefined ? null : Number(c.duty), role: role, empty: c.paired === false,
             hot: s.current === c.key }
  })
}

// Pairing's bars: every motherboard tach on one RPM scale, the one moving now in the accent (call 6).
function pairBars(s, tick) {
  var tachs = tick && Array.isArray(tick.tachs) ? tick.tachs : []
  return tachs.map(function(t) {
    return { key: String(t.key), label: String(t.label), rpm: t.rpm === null || t.rpm === undefined ? 0 : Number(t.rpm), hot: !!s && s.mover === t.key }
  })
}

function pairFound(s) {
  return (s && s.controls ? s.controls : []).filter(function(c) { return !isNvmlKey(c.key) && c.paired !== null && c.paired !== undefined })
    .map(function(c) { return c.paired ? String(c.label) + " → " + String(c.header || "") : String(c.label) + " → nothing" })
}

// Calibration's small multiples: each fan's table on its own scale (call 6), with where it is now.
function calCells(s, tick) {
  return (s && s.controls ? s.controls : []).filter(function(c) { return c.paired === true }).map(function(c) {
    var rows = setupRows({ controls: [c], current: null }, tick)[0]
    var table = (c.table || []).map(function(p) { return [Number(p[0]), Number(p[1])] })
    var top = table.reduce(function(m, p) { return Math.max(m, p[1]) }, 0)
    return { key: String(c.key), name: String(c.header || c.label), status: String(c.status || ""), table: table,
             max: Math.max(top, Number(rows.rpm) || 0, 1) * 1.06, duty: rows.duty, rpm: rows.rpm, lowest: c.lowest }
  })
}

// The review's roles (call 7): the suggestion, its reason and what calibration found, whether it still needs the user's yes.
var ROLES = ["cpu", "case", "pump", "chipset"]
var ROLE_NAME = { cpu: "CPU cooler", gpu: "GPU", pump: "Pump", chipset: "Chipset", laptop: "Laptop", case: "Case" }

function reviewRows(s) {
  return (s && s.controls ? s.controls : []).filter(function(c) { return !!c.suggestion }).map(function(c) {
    var reason = String(c.suggestion.reason) + (c.note ? " " + String(c.note) : "")
    return { key: String(c.key), name: String(c.header || c.label), role: String(c.suggestion.role), reason: reason,
             confirm: !!c.suggestion.confirm, nvml: isNvmlKey(c.key) }
  })
}

function nextRole(role) {
  var i = ROLES.indexOf(role)
  return ROLES[(i + 1) % ROLES.length]
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

// Below this, an NVIDIA fan goes back to its driver: its settable minimum, or 5 % above its measured stop when that is higher.
function nvmlFloor(control) {
  return control.floor !== null && control.floor !== undefined ? Number(control.floor) : Number(control.nvml.min)
}

// A stepped control's level for a duty, rounded up as the daemon does (§3.5): a duty within 0.05 % above a level is that level.
function levelOf(steps, d) {
  var top = Math.max(1, steps - 1)
  return Math.max(0, Math.min(top, Math.ceil((d - 0.05) * top / 100)))
}

function levelDuty(steps, level) {
  return level * 100 / Math.max(1, steps - 1)
}

function readout(t, d, control) {
  var head = Math.round(t) + " °C → " + Math.round(d) + " %"
  if (!control) return head
  if (control.nvml && d < nvmlFloor(control)) return head + " driver curve"
  if (control.stop !== null && control.stop !== undefined && d < Number(control.stop) && control.may_stop) return head + " stopped"
  if (control.levels) {
    // The fan runs the level the duty rounds up to, and its RPM is that level's.
    var level = levelOf(control.levels, d)
    var lr = rpmAt(control.calibration, levelDuty(control.levels, level))
    return head + " → level " + level + " of " + (control.levels - 1) + (lr === null ? "" : " ≈ " + formatNumber(lr, 10, 0, true) + " RPM")
  }
  var r = rpmAt(control.calibration, d)
  return r === null ? head : head + " ≈ " + formatNumber(r, 10, 0, true) + " RPM"
}

// The inspector line under the editor: what the output is doing, in words (§8.3).
function inspector(curveLive, control, controlLive, tick) {
  if (!curveLive) return ""
  if (controlLive && controlLive.state === "override") return tick && tick.full_speed ? "Full speed holds this fan at 100 %" : "An override holds this fan"
  if (controlLive && controlLive.state === "handback") {
    var floor = control && control.nvml ? nvmlFloor(control) : 30
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
  if (control.nvml) return "Driver below " + nvmlFloor(control) + " %" + (control.may_stop ? ", which may stop it" : "") + " · steps ↑" + control.step_up + " ↓" + control.step_down + " %/s"
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

// ---------------------------------------------------------------- generated curves (M8: §5.5, §11.3 call 9)

// The profiles the studio can show, in the body's order: every one but Defaults, which assigns nothing (§4.7).
function profileTabs(body) {
  var list = body && Array.isArray(body.profiles) ? body.profiles : []
  return list.filter(function(p) { return !p.builtin }).map(function(p) {
    return { value: String(p.id), label: String(p.label || capitalise(String(p.id))), running: p.id === body.profile }
  })
}

// The profile the studio shows and edits: the one chosen, else the one running, else Balanced, where saving setup's curves
// goes. Choosing one changes what the editor shows, never what runs.
function viewProfile(body, chosen) {
  var tabs = profileTabs(body)
  var has = function(id) { return tabs.some(function(t) { return t.value === id }) }
  if (chosen && has(chosen)) return String(chosen)
  if (body && has(body.profile)) return String(body.profile)
  if (has("balanced")) return "balanced"
  return tabs.length ? tabs[0].value : ""
}

function generatedDraft(body) {
  return !!(body && body.draft && body.draft.mine && Array.isArray(body.draft.generated) && body.draft.generated.length)
}

function isGenerated(body, id) {
  return !!(body && body.draft && Array.isArray(body.draft.generated) && body.draft.generated.indexOf(id) >= 0)
}

// The header's line while there are unsaved changes. Setup's curves run only once a profile they fill is on.
function draftLine(body) {
  if (!generatedDraft(body)) return "Unsaved changes, running live"
  return profileTabs(body).some(function(t) { return t.running }) ? "Generated curves, running live, not saved" : "Generated curves, not saved"
}

// Saving setup's curves switches to Balanced (§11.3 call 9), and the button says so.
function saveLabel(body) {
  return generatedDraft(body) && body.profile !== "balanced" ? "Save and switch to Balanced" : "Save"
}

// A generated curve's anchors on its temperature axis (§5.5): where its source rests, what a profile keeps it under, and its
// limit. [{ t, label }], left to right.
function anchorMarks(body, curve) {
  if (!curve || !curve.source || (curve.kind !== "graph" && curve.kind !== "trigger") || !isGenerated(body, curve.id)) return []
  var a = body.draft.anchors ? body.draft.anchors[curve.source] : null
  if (!a) return []
  return [["idle", a.idle], ["target", a.target], ["limit", a.limit]]
    .filter(function(m) { return m[1] !== null && m[1] !== undefined && Number(m[1]) >= T_MIN && Number(m[1]) <= T_MAX })
    .map(function(m) { return { t: Number(m[1]), label: m[0] + " " + Math.round(Number(m[1])) } })
}

// The load check while it runs (§5.5): the CPU against the temperature that ends a pass, its fans, and the time left.
function loadView(s) {
  var l = s ? s.load : null
  if (!l) return null
  var step = (s.steps || []).filter(function(x) { return x.step === "load-check" })[0]
  var length = step ? Number(step.seconds) : 180
  var num = function(v) { return v === null || v === undefined ? null : Number(v) }
  return {
    loading: !!l.loading,
    title: l.loading ? "Full CPU load · pass " + l.pass : "Cooling before the second pass",
    celsius: num(l.celsius),
    fans: num(l.fans),
    limit: Number(l.limit),
    ends: Number(l.limit) - 5,
    time: l.loading ? clock(Math.max(0, length - Number(l.seconds))) + " left" : clock(Number(l.seconds)),
    source: l.source ? String(l.source) : ""
  }
}

// What the review says will happen to the curves setup proposes once the roles are confirmed.
function proposalText(s, tick) {
  var p = s ? s.proposed : null
  if (!p) return ""
  if (p.refused) return String(p.refused)
  var n = (p.curves || []).length
  var on = tick && ["quiet", "balanced", "render"].indexOf(tick.profile) >= 0 ? profileLabel(tick) : ""
  var head = "Confirming opens the " + n + " curves setup made for Quiet, Balanced and Render in the editor, marked Generated. "
  if (on === "Balanced") return head + "Balanced is on, so the fans run them already; saving keeps them."
  if (on) return head + on + " is on, so the fans run them already; saving keeps them and switches to Balanced."
  return head + "Nothing runs them until you save, which switches to Balanced."
}

// Where the studio goes among `names`, the screens there are now: the one it opened on, else the focused one, else the first;
// -1 with none. A monitor that drops out and comes back keeps its name but not its screen object (§8.3).
function screenIndex(names, wanted, focused) {
  var i = names.indexOf(wanted)
  if (i < 0) i = names.indexOf(focused)
  if (i < 0 && names.length) i = 0
  return i
}

function writeBlockedReason(connected, canWrite) {
  if (!connected || canWrite) return ""
  return "Changing fans needs the penna group: sudo usermod -aG penna $USER (it applies at once)."
}

if (typeof module !== "undefined") {
  module.exports = {
    GLYPH: GLYPH, ALERT_STATES: ALERT_STATES, socketCandidates: socketCandidates, controls: controls, sensors: sensors,
    isAlert: isAlert, alerts: alerts, adopted: adopted, firmwareLines: firmwareLines, unreachableLines: unreachableLines, hasEmergency: hasEmergency, fanCount: fanCount, barState: barState,
    profileLabel: profileLabel, profileOptions: profileOptions, needsSetup: needsSetup, hero: hero, setupRunning: setupRunning,
    setupMeta: setupMeta, paired: paired, defaultsRows: defaultsRows, emptyHeaders: emptyHeaders, deviceOf: deviceOf,
    deviceGroups: deviceGroups, tierNote: tierNote, kindLine: kindLine, setupRail: setupRail, setupRows: setupRows, pairBars: pairBars,
    pairFound: pairFound, calCells: calCells, reviewRows: reviewRows, nextRole: nextRole, ROLES: ROLES, ROLE_NAME: ROLE_NAME,
    isNvmlKey: isNvmlKey, driftLine: driftLine, normalMeta: normalMeta, shortName: shortName,
    tempRows: tempRows, scale: scale, badge: badge, dutyText: dutyText, deadband: deadband, formatNumber: formatNumber,
    ago: ago, clock: clock, writeBlockedReason: writeBlockedReason, keysOf: keysOf, sameKeys: sameKeys, byKey: byKey,
    T_MIN: T_MIN, T_MAX: T_MAX, clamp: clamp, copyPoints: copyPoints, pointsEqual: pointsEqual, evalPoints: evalPoints,
    movePoint: movePoint, addPoint: addPoint, deletePoint: deletePoint, bandPolygon: bandPolygon, rpmAt: rpmAt, readout: readout,
    inspector: inspector, limitsLine: limitsLine, curveOptions: curveOptions, handlesOf: handlesOf, curveWith: curveWith,
    moveHandle: moveHandle, pathOf: pathOf, shadeOf: shadeOf, nvmlFloor: nvmlFloor, profileTabs: profileTabs,
    viewProfile: viewProfile, generatedDraft: generatedDraft, isGenerated: isGenerated, draftLine: draftLine, saveLabel: saveLabel,
    anchorMarks: anchorMarks, loadView: loadView, proposalText: proposalText, screenIndex: screenIndex,
    levelOf: levelOf, levelDuty: levelDuty
  }
}
