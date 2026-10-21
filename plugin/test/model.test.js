const test = require("node:test")
const assert = require("node:assert/strict")
const M = require("../Model.js")

const cpu = { key: "hwmon/k10temp/0000:00:18.3/temp1", label: "Tctl", device: "CPU", celsius: 62.4, scale: [20, 95] }
const gpu = { key: "nvml/GPU-1/temp", label: "GPU", device: "NVIDIA GeForce RTX 3090", celsius: 41.2, scale: [20, 100] }
const nvme = { key: "hwmon/nvme/0000:04:00.0/temp1", label: "Composite", device: "NVMe", celsius: 37.9, scale: [20, 90] }
const acpi = { key: "hwmon/acpitz/thermal_zone0/temp1", label: "temp1", device: "ACPI thermal zone", celsius: 16.8, below_ambient: true }

function control(over) {
  return Object.assign({ key: "hwmon/it87/it87.2608/pwm1", label: "CPU cooler", kind: "pwm", fans: 1, rpm: 1140, state: "curve",
                         duty: 51, curve: "cpu-quiet", curve_label: "CPU quiet", override: null }, over)
}

function tick(over) {
  return Object.assign({
    profile: "balanced",
    profiles: [{ id: "quiet", label: "Quiet" }, { id: "balanced", label: "Balanced" }, { id: "render", label: "Render" }],
    full_speed: false, emergency: [], sensors: [cpu, gpu, nvme, acpi],
    controls: [control(), control({ key: "k2", label: "Front intake", fans: 3, curve: "case" })],
    curves: [{ id: "cpu-quiet", source: cpu.key }, { id: "gpu", source: gpu.key }, { id: "case", kind: "mix" }]
  }, over)
}

test("socket candidates follow §7.3 and drop duplicates", () => {
  assert.deepEqual(M.socketCandidates("", "/run/user/1000/"), ["/run/penna/penna.sock", "/run/user/1000/penna/penna.sock"])
  assert.deepEqual(M.socketCandidates("/tmp/p.sock", ""), ["/tmp/p.sock", "/run/penna/penna.sock"])
  assert.deepEqual(M.socketCandidates("/run/penna/penna.sock", ""), ["/run/penna/penna.sock"])
})

test("the bar glyph changes shape for every state that matters", () => {
  assert.equal(M.barState(tick(), false).glyph, M.GLYPH.off)
  assert.equal(M.barState(tick(), true).glyph, M.GLYPH.fan)
  assert.equal(M.barState(tick({ full_speed: true }), true).glyph, M.GLYPH.full)
  assert.equal(M.barState(tick({ controls: [control({ override: { duty: 80, until: 99 } })] }), true).glyph, M.GLYPH.timed)
  assert.equal(M.barState(tick({ controls: [control({ override: { duty: 80, until: null } })] }), true).glyph, M.GLYPH.full)
  assert.equal(M.barState(tick({ controls: [] }), true).glyph, M.GLYPH.auto)
  for (const state of M.ALERT_STATES) {
    const b = M.barState(tick({ full_speed: true, controls: [control({ state })] }), true)
    assert.deepEqual(b, { glyph: M.GLYPH.alert, tone: "urgent" }, state)
  }
  assert.equal(M.barState(tick({ emergency: [{ label: "CPU · Tctl", celsius: 94.5, release: 85 }] }), true).tone, "urgent")
})

test("the hero names what is wrong, or the profile", () => {
  assert.equal(M.hero(tick(), true, 0, 0).title, "Balanced")
  assert.equal(M.hero(tick(), true, 0, 0).meta, null)
  const em = M.hero(tick({ emergency: [{ label: "CPU · Tctl", celsius: 94.5, release: 85 }] }), true, 0, 0)
  assert.equal(em.title, "CPU · Tctl at 95 °C")
  assert.equal(em.meta, "EVERY FAN AT 100 % UNTIL 85 °C")
  assert.equal(M.hero(tick({ controls: [control({ label: "Rear exhaust", state: "contested" })] }), true, 0, 0).title, "Rear exhaust is contested")
  assert.equal(M.hero(tick(), false, 112, 100).meta, "LAST UPDATE 12 S AGO")
  assert.equal(M.hero(null, false, 0, 0).meta, "NOT CONNECTED")
})

test("temperature rows are the sources curves read, never the cold ACPI zone", () => {
  const rows = M.tempRows(tick())
  assert.deepEqual(rows.map((r) => r.name), ["CPU", "GPU"])
  assert.deepEqual(rows.map((r) => r.detail), ["Tctl", ""], "a GPU sensor called GPU is not 'GPU · GPU'")
  assert.deepEqual(rows[0].lo, 20)
  const fallback = M.tempRows(tick({ curves: [] }))
  assert.deepEqual(fallback.map((r) => r.name), ["CPU", "GPU", "NVMe"])
  assert.equal(M.normalMeta(M.fanCount(tick()), [{ name: "CPU", text: "62 °C" }, { name: "GPU", text: "41 °C" }]), "4 FANS · CPU 62 °C · GPU 41 °C")
})

test("badges and duty follow the state vocabulary", () => {
  assert.deepEqual(M.badge(control({ state: "handback" }), tick(), 0), { text: "DRIVER", tone: "dim" })
  assert.deepEqual(M.badge(control({ state: "override", override: { duty: 100, until: null } }), tick({ full_speed: true }), 0), { text: "FULL", tone: "accent" })
  assert.deepEqual(M.badge(control({ state: "override", override: { duty: 80, until: 130 } }), tick(), 10), { text: "2:00", tone: "accent" })
  assert.deepEqual(M.badge(control({ state: "contested" }), tick(), 0), { text: "CONTESTED", tone: "urgent" })
  assert.equal(M.badge(control(), tick(), 0), null)
  assert.equal(M.dutyText(control({ state: "handback" })), "—")
  assert.equal(M.dutyText(control({ duty: 50.6 })), "51 %")
})

test("the dead band holds a value sitting on a boundary", () => {
  let shown = M.deadband(null, 62.4, 1)
  assert.equal(shown, 62)
  for (const v of [62.5, 62.45, 62.55, 62.59]) shown = M.deadband(shown, v, 1)
  assert.equal(shown, 62, "flickering around 62.5 never changes the text")
  shown = M.deadband(shown, 62.61, 1)
  assert.equal(shown, 63)
  assert.equal(M.deadband(1140, 1143, 10), 1140)
  assert.equal(M.deadband(1140, 1147, 10), 1150)
  assert.equal(M.deadband(5, NaN, 1), 5)
})

test("numbers format with grouping, steps and decimals", () => {
  assert.equal(M.formatNumber(1143, 10, 0, true), "1,140")
  assert.equal(M.formatNumber(62.04, 0.1, 1, false), "62.0")
  assert.equal(M.formatNumber(NaN, 1, 0, false), "—")
  assert.equal(M.ago(3), "JUST NOW")
  assert.equal(M.ago(300), "5 MIN AGO")
  assert.equal(M.clock(59.2), "1:00")
})

test("writes blocked by the daemon say how to fix it", () => {
  assert.equal(M.writeBlockedReason(true, true), "")
  assert.equal(M.writeBlockedReason(false, false), "")
  assert.match(M.writeBlockedReason(true, false), /penna group/)
})
