const test = require("node:test")
const assert = require("node:assert/strict")
const M = require("../Model.js")

const pts = [[45, 30], [65, 40], [78, 65], [88, 100]]
const cooler = { key: "pwm1", nvml: null, floor: 30, stop: 20, may_stop: false, step_up: 20, step_down: 5,
                 calibration: [[20, 420], [40, 765], [60, 1110], [80, 1455], [100, 1800]] }
const gpuFan = { key: "fan0", nvml: { min: 30, max: 100 }, floor: 30, may_stop: true, step_up: 20, step_down: 5, calibration: null }

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
  assert.equal(M.readout(40, 40, gpuFan), "40 °C → 40 %", "no RPM axis before calibration")
  assert.equal(M.rpmAt(cooler.calibration, 10), 0)
  assert.equal(M.rpmAt(null, 50), null)
})

test("the inspector says what the output is doing", () => {
  assert.equal(M.inspector({ held: true, hold_until: 60.4 }, cooler, { state: "curve", trend: "steady" }, {}), "Holding: hysteresis keeps it until 60.4 °C")
  assert.equal(M.inspector({ held: false }, cooler, { state: "curve", trend: "rising", target: 64 }, {}), "Rising at the step limit, 20 %/s → 64 %")
  assert.equal(M.inspector({ held: false }, gpuFan, { state: "handback", rpm: 0 }, {}), "Below the 30 % floor: the driver runs this fan (0 RPM)")
  assert.equal(M.inspector({ held: false }, cooler, { state: "curve", trend: "steady" }, {}), "Following the curve")
  assert.equal(M.limitsLine(cooler), "min 30 % · stalls below 20 % · steps ↑20 ↓5 %/s")
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
