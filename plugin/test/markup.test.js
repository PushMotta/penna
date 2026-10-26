// The daemon's names are text, never markup (Model.escapeMarkup): any member of the penna group can set a label, and a Text
// that read it as markup would fetch an <img> from another user's desktop, or restyle the line.
const test = require("node:test")
const assert = require("node:assert/strict")
const fs = require("node:fs")
const path = require("node:path")
const M = require("../Model.js")

const root = path.join(__dirname, "..")
const qml = []
;(function walk(dir) {
  for (const name of fs.readdirSync(dir)) {
    const p = path.join(dir, name)
    if (name === "node_modules" || name.startsWith(".")) continue
    if (fs.statSync(p).isDirectory()) walk(p)
    else if (name.endsWith(".qml")) qml.push(p)
  }
})(root)

// Each `Text {` block in a file, with its first line number and its whole source.
function textBlocks(file) {
  const lines = fs.readFileSync(file, "utf8").split("\n")
  const out = []
  lines.forEach((line, i) => {
    if (!/^\s*Text\s*\{/.test(line)) return
    let depth = 0, body = ""
    for (let j = i; j < lines.length; j++) {
      body += lines[j] + "\n"
      depth += (lines[j].match(/\{/g) || []).length - (lines[j].match(/\}/g) || []).length
      if (depth <= 0) break
    }
    out.push({ where: path.relative(root, file) + ":" + (i + 1), body })
  })
  return out
}

test("a label is shown as the characters it holds", () => {
  const label = "<img src=\"http://example.invalid/x.png\">Fan & \"co\""
  assert.equal(M.escapeMarkup(label), "&lt;img src=&quot;http://example.invalid/x.png&quot;&gt;Fan &amp; &quot;co&quot;")
  const line = M.dimmedLine(label, "<b>×2</b>", "#888888", true)
  assert.ok(!/<img|<b>/.test(line), line)
  assert.equal(line, M.escapeMarkup(label) + " <font color=\"#888888\"><small>&lt;b&gt;×2&lt;/b&gt;</small></font>")
})

test("ordinary names look as they did", () => {
  assert.equal(M.dimmedLine("Front intake", "×3", "#7f7f7f", true), "Front intake <font color=\"#7f7f7f\"><small>×3</small></font>")
  assert.equal(M.dimmedLine("Tctl", "· CPU", "#7f7f7f", false), "Tctl <font color=\"#7f7f7f\">· CPU</font>")
  assert.equal(M.dimmedLine("CPU cooler", "", "#7f7f7f", true), "CPU cooler")
  assert.equal(M.dimmedLine(42, null, "#7f7f7f", false), "42")
})

test("every Text says how it reads its text, and only Model.dimmedLine writes markup", () => {
  const blocks = qml.flatMap(textBlocks)
  assert.ok(blocks.length > 50, "found the plugin's Text elements")
  // Qt's default, AutoText, reads anything that looks like a tag as markup.
  const unstated = blocks.filter((b) => !/textFormat\s*:/.test(b.body)).map((b) => b.where)
  assert.deepEqual(unstated, [], "Text without a textFormat")
  for (const b of blocks.filter((b) => /textFormat\s*:\s*Text\.(StyledText|RichText|MarkdownText)/.test(b.body))) {
    const text = (b.body.match(/\btext\s*:([\s\S]*?)\n\s*(color|font\.|elide|wrapMode|anchors|width|\})/) || [])[1] || ""
    assert.match(text, /Model\.dimmedLine\(/, b.where + " builds its markup through Model.dimmedLine")
  }
  // No QML file writes a tag by hand.
  for (const f of qml) {
    const src = fs.readFileSync(f, "utf8")
    assert.ok(!/["']\s*<\/?(font|small|b|i|img|a)\b/.test(src), path.relative(root, f) + " writes markup by hand")
  }
})
