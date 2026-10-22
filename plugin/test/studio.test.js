const test = require("node:test")
const assert = require("node:assert/strict")
const M = require("../Model.js")

const pts = [[45, 30], [65, 40], [78, 65], [88, 100]]
const cooler = { key: "pwm1", nvml: null, floor: 30, stop: 20, may_stop: false, step_up: 20, step_down: 5,
                 calibration: [[20, 420], [40, 765], [60, 1110], [80, 1455], [100, 1800]] }
// The 3090's fan as setup measured it: settable from 30 %, but standing still below 40 % in manual.
const gpuFan = { key: "fan0", nvml: { min: 30, max: 100 }, stop: 40, floor: 40, may_stop: true, step_up: 20, step_down: 5, calibration: null }

test("a dragged point snaps, stays between its neighbours and never makes the curve dip", () => {
  assert.deepEqual(M.movePoint(pts, 1, 70.4, 39.6), [[45, 30], [70, 40], [78, 65], [88, 100]])
  assert.deepEqual(M.movePoint(pts, 1, 90, 40)[1], [77, 40], "stops a degree short of the next point")
  assert.deepEqual(M.movePoint(pts, 1, 65, 80), [[45, 30], [65, 80], [78, 80], [88, 100]], "pushes later points up")
  assert.deepEqual(M.movePoint(pts, 2, 78, 10), [[45, 10], [65, 10], [78, 10], [88, 100]], "pushes earlier points down")
  assert.deepEqual(M.movePoint(pts, 0, 5, -20)[0], [20, 0])
  assert.deepEqual(pts[1], [65, 40], "the input is never mutated")
})

test("points are added halfway and removed down to two", () => {
  assert.deepEqual(M.addPoint(pts, 1), { points: [[45, 30], [65, 40], [72, 53], [78, 65], [88, 100]], index: 2 })
  assert.equal(M.addPoint([[45, 30], [46, 40]], 0).points.length, 2, "no whole degree of room")
  assert.equal(M.addPoint(pts, 3).points.length, 4, "nothing after the last point")
  assert.deepEqual(M.deletePoint(pts, 3), { points: [[45, 30], [65, 40], [78, 65]], index: 2 })
  assert.equal(M.deletePoint([[45, 30], [88, 100]], 0).points.length, 2)
})

test("the curve evaluates flat beyond its ends", () => {
  assert.equal(M.evalPoints(pts, 20), 30)
  assert.equal(M.evalPoints(pts, 55), 35)
  assert.equal(M.evalPoints(pts, 100), 100)
})

test("the hysteresis band lies between f(T) and f(T + H)", () => {
  const band = M.bandPolygon(pts, 3, 1)
  assert.equal(band.length, 162)
  assert.deepEqual(band[0], [20, 30])
  const at70 = band.find((p, i) => i < 81 && p[0] === 70)
  assert.ok(Math.abs(at70[1] - M.evalPoints(pts, 73)) < 1e-9)
  assert.deepEqual(M.bandPolygon(pts, 0, 1), [])
})

test("the readout names temperature, duty and what it buys", () => {
  assert.equal(M.readout(72, 58, cooler), "72 °C → 58 % ≈ 1,080 RPM")
  assert.equal(M.readout(40, 20, gpuFan), "40 °C → 20 % driver curve")
  assert.equal(M.readout(50, 35, gpuFan), "50 °C → 35 % driver curve", "settable, but below the measured stop")
  assert.equal(M.readout(40, 40, gpuFan), "40 °C → 40 %", "no RPM axis before calibration")
  assert.equal(M.rpmAt(cooler.calibration, 10), 0)
  assert.equal(M.rpmAt(null, 50), null)
})

test("the inspector says what the output is doing", () => {
  assert.equal(M.inspector({ held: true, hold_until: 60.4 }, cooler, { state: "curve", trend: "steady" }, {}), "Holding: hysteresis keeps it until 60.4 °C")
  assert.equal(M.inspector({ held: false }, cooler, { state: "curve", trend: "rising", target: 64 }, {}), "Rising at the step limit, 20 %/s → 64 %")
  assert.equal(M.inspector({ held: false }, gpuFan, { state: "handback", rpm: 0 }, {}), "Below the 40 % floor: the driver runs this fan (0 RPM)")
  assert.equal(M.inspector({ held: false }, cooler, { state: "curve", trend: "steady" }, {}), "Following the curve")
  assert.equal(M.limitsLine(cooler), "min 30 % · stalls below 20 % · steps ↑20 ↓5 %/s")
  assert.equal(M.limitsLine(gpuFan), "Driver below 40 %, which may stop it · steps ↑20 ↓5 %/s", "the measured stop, not the driver's 30 %")
})

test("triggers and flats edit through the same handles", () => {
  const trig = { id: "exhaust", kind: "trigger", trigger: { idle: 35, load: 70, on: 70, off: 60 } }
  const h = M.handlesOf(trig)
  assert.deepEqual(h, [[60, 35], [70, 70]])
  assert.deepEqual(M.moveHandle("trigger", h, 0, 75, 90), [[69, 70], [70, 70]], "off stays below on, idle never above load")
  assert.deepEqual(M.moveHandle("trigger", h, 1, 50, 10), [[60, 35], [61, 35]])
  assert.deepEqual(M.curveWith(trig, [[58, 30], [72, 75]]).trigger, { idle: 30, load: 75, off: 58, on: 72 })
  assert.deepEqual(M.pathOf("trigger", h), [[20, 35], [70, 35], [70, 70], [100, 70]])
  assert.deepEqual(M.shadeOf(trig, h), [[60, 35], [70, 35], [70, 70], [60, 70]])
  const flat = { id: "idle", kind: "flat", duty: 35 }
  assert.deepEqual(M.moveHandle("flat", M.handlesOf(flat), 0, 90, 41.6), [[60, 42]])
  assert.equal(M.curveWith(flat, [[60, 42]]).duty, 42)
  assert.equal(trig.trigger.idle, 35, "curveWith never mutates its input")
})

// Setup's draft on a new install, as the studio body carries it (§5.5, §11.3 call 9): Defaults on, three profiles generated.
const CPU = "hwmon/k10temp/0000:00:18.3/temp1"
const GPU = "nvml/GPU-1/temp"
const generated = {
  profile: "defaults",
  profiles: [{ id: "defaults", label: "Defaults", builtin: true, assign: {} },
             { id: "quiet", label: "Quiet", assign: {} }, { id: "balanced", label: "Balanced", assign: {} }, { id: "render", label: "Render", assign: {} }],
  curves: [],
  draft: { mine: true, dirty: true, curves: ["cpu-balanced"], controls: [], generated: ["cpu-balanced", "gpu-balanced"],
           anchors: { [CPU]: { idle: 41.3, target: null, limit: 90 }, [GPU]: { idle: 45, target: 83, limit: 93 } } }
}

test("the studio shows a profile without running it, Balanced while Defaults runs", () => {
  assert.deepEqual(M.profileTabs(generated).map(t => t.value), ["quiet", "balanced", "render"], "never Defaults")
  assert.equal(M.viewProfile(generated, ""), "balanced", "where saving goes")
  assert.equal(M.viewProfile(generated, "quiet"), "quiet")
  assert.equal(M.viewProfile(generated, "defaults"), "balanced", "Defaults assigns nothing to show")
  assert.equal(M.viewProfile({ ...generated, profile: "render" }, ""), "render", "the one running, when it can be shown")
  assert.equal(M.viewProfile({ profile: "defaults", profiles: [generated.profiles[0]] }, ""), "", "nothing to show")
})

test("setup's curves are marked Generated until saved, and Save says it switches to Balanced", () => {
  assert.equal(M.isGenerated(generated, "cpu-balanced"), true)
  assert.equal(M.isGenerated(generated, "gpu"), false)
  assert.equal(M.draftLine(generated), "Generated curves, not saved", "Defaults runs nothing of them")
  assert.equal(M.draftLine({ ...generated, profile: "balanced" }), "Generated curves, running live, not saved")
  assert.equal(M.saveLabel(generated), "Save and switch to Balanced")
  assert.equal(M.saveLabel({ ...generated, profile: "balanced" }), "Save")
  const edits = { ...generated, draft: { ...generated.draft, generated: [] } }
  assert.equal(M.draftLine(edits), "Unsaved changes, running live")
  assert.equal(M.saveLabel(edits), "Save")
})

test("a generated curve's anchors go on its temperature axis", () => {
  const cpu = { id: "cpu-balanced", kind: "graph", source: CPU, points: [[51, 15], [82, 100]] }
  assert.deepEqual(M.anchorMarks(generated, cpu), [{ t: 41.3, label: "idle 41" }, { t: 90, label: "limit 90" }], "no target for a CPU")
  assert.deepEqual(M.anchorMarks(generated, { ...cpu, id: "gpu-balanced", source: GPU }).map(m => m.label), ["idle 45", "target 83", "limit 93"])
  assert.deepEqual(M.anchorMarks(generated, { ...cpu, id: "mine" }), [], "a curve setup didn't make has none")
  assert.deepEqual(M.anchorMarks(generated, { id: "pump-balanced", kind: "flat", duty: 85 }), [])
})

test("the load check shows the CPU against where a pass ends, and the time left", () => {
  const s = { steps: [{ step: "load-check", seconds: 180 }],
              load: { source: CPU, pass: 1, loading: true, seconds: 50.4, celsius: 64.9, fans: 52.4, limit: 90, result: null } }
  assert.deepEqual(M.loadView(s), { loading: true, title: "Full CPU load · pass 1", celsius: 64.9, fans: 52.4, limit: 90, ends: 85,
                                    time: "2:10 left", source: CPU })
  const cooling = M.loadView({ ...s, load: { ...s.load, pass: 2, loading: false, seconds: 34, celsius: null } })
  assert.deepEqual([cooling.title, cooling.time, cooling.celsius], ["Cooling before the second pass", "0:34", null])
  assert.equal(M.loadView({ steps: [] }), null)
})

test("the review says what confirming does with the proposed curves", () => {
  const s = { proposed: { curves: new Array(19).fill("c"), anchors: {}, notes: [], refused: null } }
  assert.equal(M.proposalText(s, { profile: "defaults" }),
    "Confirming opens the 19 curves setup made for Quiet, Balanced and Render in the editor, marked Generated. Nothing runs them until you save, which switches to Balanced.")
  assert.match(M.proposalText(s, { profile: "balanced", profiles: [{ id: "balanced", label: "Balanced" }] }), /Balanced is on, so the fans run them already; saving keeps them\.$/)
  assert.equal(M.proposalText({ proposed: { refused: "Another window has unsaved changes." } }, {}), "Another window has unsaved changes.")
  assert.equal(M.proposalText({}, {}), "")
})
