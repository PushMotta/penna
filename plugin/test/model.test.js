const test = require("node:test")
const assert = require("node:assert/strict")
const M = require("../Model.js")

const cpu = { key: "hwmon/k10temp/0000:00:18.3/temp1", label: "Tctl", device: "CPU", celsius: 62.4, scale: [20, 95] }
const gpu = { key: "nvml/GPU-1/temp", label: "GPU", device: "NVIDIA GeForce RTX 3090", celsius: 41.2, scale: [20, 100] }
const nvme = { key: "hwmon/nvme/0000:04:00.0/temp1", label: "Composite", device: "NVMe", celsius: 37.9, scale: [20, 90] }
const acpi = { key: "hwmon/acpitz/thermal_zone0/temp1", label: "temp1", device: "ACPI thermal zone", celsius: 16.8, below_ambient: true }

function control(over) {
  return Object.assign({ key: "hwmon/it87/it87.2608/pwm1", label: "CPU cooler", kind: "duty", fans: 1, rpm: 1140, state: "curve",
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
  assert.deepEqual(M.socketCandidates("/tmp/p.sock", "/run/user/1000"), ["/tmp/p.sock"], "a socket named on purpose is the only one tried")
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

test("Defaults says who runs the fans, and still names an emergency first", () => {
  const firmware = [control({ state: "firmware" })]
  const d = M.hero(tick({ profile: "defaults", controls: firmware }), true, 0, 0)
  assert.deepEqual([d.title, d.meta, d.glyph], ["Defaults", "THE BIOS AND DRIVERS RUN EVERY FAN", M.GLYPH.auto])
  const em = M.hero(tick({ profile: "defaults", controls: firmware, emergency: [{ label: "CPU · Tctl", celsius: 94.5, release: 85 }] }), true, 0, 0)
  assert.equal(em.tone, "urgent")
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

test("the popup says when the studio is previewing unsaved changes", () => {
  const h = M.hero(tick({ draft: { curves: ["cpu-quiet"], controls: [] } }), true, 0, 0)
  assert.equal(h.meta, "PREVIEWING UNSAVED CHANGES FROM THE STUDIO")
  assert.equal(M.hero(tick({ draft: null }), true, 0, 0).meta, null)
})

test("configured fans on firmware fold away, and nothing adopted reads as firmware running every fan", () => {
  const firstStart = tick({
    controls: [control({ key: "fan0", label: "GPU fan 1", state: "firmware", curve: null }),
               control({ key: "fan1", label: "GPU fan 2", state: "absent", curve: null })],
    firmware: [{ key: "pwm4", label: "pwm4" }],
    unreachable: [{ id: "pci/0a", label: "RTX 2060 SUPER", reason: "passed through to a VM (vfio-pci)" }]
  })
  assert.equal(M.adopted(firstStart).length, 0)
  assert.equal(M.barState(firstStart, true).glyph, M.GLYPH.auto)
  assert.equal(M.hero(firstStart, true, 0, 0).title, "Firmware runs every fan")
  assert.deepEqual(M.firmwareLines(firstStart), ["GPU fan 1", "pwm4"])
  assert.deepEqual(M.unreachableLines(firstStart), ["GPU fan 2 · configured, but not on this machine now", "RTX 2060 SUPER · passed through to a VM (vfio-pci)"])
  assert.equal(M.fanCount(firstStart), 0)
})

// ---------------------------------------------------------------- design review 03 (§11.3)

const it8688 = "hwmon/it8688_4008090b/it87.2624"
const tachs = [
  { key: it8688 + "/fan1", label: "CPU_FAN", device: "IT8688E", rpm: 1196, control: null },
  { key: it8688 + "/fan4", label: "PCH_FAN", device: "IT8688E", rpm: 0, control: null },
  { key: "hwmon/it8792_4008090b/it87.2656/fan2", label: "SYS_FAN6_PUMP", device: "IT8792E", rpm: 0, control: null }
]
const gpuFan = control({ key: "nvml/GPU-1/fan0", label: "GPU fan 1", state: "firmware", rpm: 0, duty: 0, curve: null, curve_label: null, tier: "verified" })

test("pills: Defaults first, then only profiles that run a fan; a new install has none to choose (calls 1, 2)", () => {
  const profiles = [{ id: "defaults", label: "Defaults", builtin: true, fans: 0 }, { id: "quiet", label: "Quiet", fans: 0 },
                    { id: "balanced", label: "Balanced", fans: 9 }, { id: "render", label: "Render", fans: 0 }]
  assert.deepEqual(M.profileOptions(tick({ profiles })).map(p => p.value), ["defaults", "balanced"])
  assert.equal(M.profileOptions(tick({ profiles }))[0].builtin, true)
  assert.deepEqual(M.profileOptions(tick({ profiles, profile: "render" })).map(p => p.value), ["defaults", "balanced", "render"], "the active one shows")
  const fresh = profiles.map(p => Object.assign({}, p, { fans: 0 }))
  assert.equal(M.needsSetup(tick({ profiles: fresh, profile: "defaults" })), true)
  assert.equal(M.needsSetup(tick({ profiles })), false)
  assert.equal(M.profileOptions(tick()).length, 3, "a daemon that doesn't count fans shows every profile")
})

test("Defaults lists every fan by its header, with its RPM and who runs it, and no duty (call 3)", () => {
  const t = tick({ profile: "defaults", tachs, controls: [gpuFan], firmware: [{ key: "nvml/GPU-1/fan1", label: "GPU fan 2", rpm: 0 }] })
  const rows = M.defaultsRows(t)
  assert.deepEqual(rows.map(r => [r.label, r.curve_label, r.rpm, r.duty]),
    [["CPU_FAN", "BIOS", 1196, null], ["PCH_FAN", "BIOS", 0, null], ["SYS_FAN6_PUMP", "BIOS", 0, null], ["GPU fan 1", "Driver", 0, null], ["GPU fan 2", "Driver", 0, null]])
  assert.deepEqual(M.emptyHeaders(t), [], "before pairing nothing is known to be empty")
  const after = tick({ profile: "defaults", tachs: tachs.map(x => Object.assign({}, x, { control: x.label === "SYS_FAN6_PUMP" ? null : x.key.replace("fan", "pwm") })) })
  assert.deepEqual(M.defaultsRows(after).map(r => r.label), ["CPU_FAN", "PCH_FAN"])
  assert.deepEqual(M.emptyHeaders(after), ["SYS_FAN6_PUMP · no fan turns on it"])
})

test("while setup runs, the bar shows the timed glyph and the hero says how far (call 4)", () => {
  const setup = { step: "pairing", left: 102, stopped: null, waiting: null, controls: [
    { key: it8688 + "/pwm1", write: "ok", paired: true }, { key: it8688 + "/pwm2", write: "ok", paired: null },
    { key: it8688 + "/pwm3", write: "reverts", paired: null }, { key: "nvml/GPU-1/fan0", write: "ok", paired: true }] }
  assert.deepEqual(M.barState(tick({ setup }), true), { glyph: M.GLYPH.timed, tone: "accent" })
  const h = M.hero(tick({ setup }), true, 0, 0)
  assert.deepEqual([h.title, h.meta], ["Setting up fans", "PAIRING · 2 OF 2 · ABOUT 1:42 LEFT"])
  assert.equal(M.hero(tick({ setup: Object.assign({}, setup, { stopped: "You stopped setup." }) }), true, 0, 0).title, "Setup stopped")
  assert.equal(M.hero(tick({ setup: Object.assign({}, setup, { step: "review" }) }), true, 0, 0).meta, "CONFIRM THE ROLES IN THE STUDIO")
  const alert = M.barState(tick({ setup, emergency: [{ label: "CPU · Tctl", celsius: 94.5, release: 85 }] }), true)
  assert.equal(alert.tone, "urgent", "an emergency still outranks setup")
})

test("cards are grouped under their device, which names the tier once (call 10)", () => {
  const t = tick({ tachs, sensors: [cpu, gpu], controls: [
    control({ key: it8688 + "/pwm1", label: "CPU_FAN", tier: "verified" }), gpuFan,
    control({ key: "hwmon/thinkpad/thinkpad_hwmon/pwm1", label: "Fan", kind: "levels", tier: "documented", class: "laptop" })] })
  const g = M.deviceGroups(M.controls(t), t)
  assert.deepEqual(g.map(x => [x.heading, x.tier, x.keys.length]),
    [["IT8688E · MOTHERBOARD", "Verified", 1], ["RTX 3090 · NVML", "Verified", 1], ["THINKPAD · LAPTOP", "Documented", 1]])
  assert.equal(g[2].note, "Nobody has verified this driver with Penna yet.")
  assert.equal(M.kindLine(M.controls(t)[0]), "", "plain duty adds no line")
  assert.equal(M.kindLine(M.controls(t)[2]), "Fixed levels. The curve is rounded to a level.")
})

test("the setup view's rail, rows, bars and review come from the run's progress (calls 4–7)", () => {
  const s = { step: "pairing", left: 90, current: it8688 + "/pwm1", mover: it8688 + "/fan1",
    steps: [{ step: "idle", seconds: 30 }, { step: "write-test", seconds: 5 }, { step: "pairing", seconds: 102 }, { step: "roles", seconds: 0 },
            { step: "calibration", seconds: 90 }, { step: "review", seconds: 0 }],
    controls: [{ key: it8688 + "/pwm1", label: "IT8688E pwm1", header: "CPU_FAN", status: "100 % for 6 s", duty: 100, paired: null, tachs: [it8688 + "/fan1"],
                 table: [], suggestion: null }] }
  assert.deepEqual(M.setupRail(s).map(r => [r.label, r.state]),
    [["1 Idle sample · 30 s", "done"], ["2 Write test · 5 s", "done"], ["3 Pairing · 1:42", "on"], ["4 Roles", "todo"], ["5 Calibration · 1:30", "todo"], ["6 Review", "todo"]])
  const row = M.setupRows(s, tick({ tachs }))[0]
  assert.deepEqual([row.name, row.rpm, row.duty, row.hot], ["CPU_FAN", 1196, 100, true])
  assert.deepEqual(M.pairBars(s, tick({ tachs })).map(b => [b.label, b.hot]), [["CPU_FAN", true], ["PCH_FAN", false], ["SYS_FAN6_PUMP", false]])
  const review = { step: "review", controls: [
    { key: it8688 + "/pwm4", label: "IT8688E pwm4", header: "PCH_FAN", suggestion: { role: "chipset", reason: "The board calls this header PCH_FAN.", confirm: false } },
    { key: it8688 + "/pwm2", label: "IT8688E pwm2", header: "SYS_FAN1", suggestion: { role: "case", reason: "A system fan header.", confirm: false },
      note: "At 0 % it turned faster, 2,170 RPM, than at 5 %: Penna never runs it below 5 %." },
    { key: "hwmon/it8792_4008090b/it87.2656/pwm1", label: "IT8792E pwm1", header: "SYS_FAN5_PUMP", suggestion: { role: "case", reason: "On a pump header…", confirm: true } },
    { key: "hwmon/it8792_4008090b/it87.2656/pwm2", label: "IT8792E pwm2", header: null, paired: false, suggestion: null }] }
  assert.deepEqual(M.reviewRows(review).map(r => [r.name, r.role, r.confirm]),
    [["PCH_FAN", "chipset", false], ["SYS_FAN1", "case", false], ["SYS_FAN5_PUMP", "case", true]])
  assert.equal(M.reviewRows(review)[1].reason, "A system fan header. At 0 % it turned faster, 2,170 RPM, than at 5 %: Penna never runs it below 5 %.")
  assert.equal(M.nextRole("case"), "pump")
  assert.equal(M.nextRole("chipset"), "cpu")
})

test("a drifting fan says how much slower, and nothing when it turns as calibrated (§5.6)", () => {
  assert.equal(M.driftLine(control({ drift: 0.3 })), "Turns 30 % slower than when it was set up: dust, or a tired bearing.")
  assert.equal(M.driftLine(control({ drift: null })), "")
  assert.equal(M.driftLine(control()), "")
})

test("a profile that sets the power profile shows as a pill on a laptop with no other lever", () => {
  const tick = { profile: "defaults", profiles: [
    { id: "defaults", label: "Defaults", builtin: true, fans: 0 },
    { id: "quiet", label: "Quiet", fans: 0, power: "quiet" },
    { id: "balanced", label: "Balanced", fans: 0, power: "balanced" },
    { id: "render", label: "Render", fans: 0 } ] }
  assert.deepEqual(M.profileOptions(tick).map(p => p.value), ["defaults", "quiet", "balanced"])
  assert.equal(M.needsSetup(tick), false)
})

test("a pennad that doesn't fit this plugin is named, with what to update", () => {
  assert.equal(M.daemonProblem(null), "")
  assert.equal(M.daemonProblem({ v: 2, daemon: "0.10.0" }), "")
  assert.equal(M.daemonProblem({ v: 2, daemon: "0.9.0" }), "")
  assert.match(M.daemonProblem({ v: 2, daemon: "0.8.3" }), /0\.8\.3 is older .* Update the penna package/)
  assert.match(M.daemonProblem({ v: 3, daemon: "1.0.0" }), /omarchy plugin update io\.github\.pushmotta\.penna/)
  assert.match(M.daemonProblem({ v: 1, daemon: "0.2.0" }), /Update the penna package/)
  assert.equal(M.olderThan("0.10.0 (1e0255d)", "0.9.0"), false, "numbers, not text: 10 is after 9")
  assert.equal(M.olderThan("0.9.0", "0.10.0"), true)
  assert.equal(M.olderThan("", "0.9.0"), true, "an unreadable version is older")
})

test("a temperature row says its device is asleep", () => {
  const tick = {
    sensors: [{ key: "hwmon/amdgpu/0000:03:00.0/temp1", label: "edge", device: "AMD GPU", celsius: null, asleep: true, scale: [20, 100] },
              { key: "hwmon/k10temp/0000:00:18.3/temp1", label: "Tctl", device: "AMD CPU", celsius: 41.2, scale: [20, 100] }],
    curves: [{ id: "dgpu", source: "hwmon/amdgpu/0000:03:00.0/temp1" }, { id: "cpu", source: "hwmon/k10temp/0000:00:18.3/temp1" }],
  }
  const rows = M.tempRows(tick)
  assert.deepEqual(rows.map(r => [r.asleep, r.error, r.celsius]), [[true, "", null], [false, "", 41.2]])
})
