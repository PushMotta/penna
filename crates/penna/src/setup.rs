//! `penna setup` (DESIGN.md §5, §11.3): the same run as the studio's setup view, in a terminal. It holds its own connection, so
//! Ctrl-C, or a closed terminal, stops setup and every fan goes back as it was found. One line per step as it ends, a live line
//! while it runs, then the roles to confirm and the curves setup proposes, a draft on this connection, to save or leave.
use crate::client::{self, Client};
use crate::render::Style;
use anyhow::{Context, Result, anyhow, bail};
use penna_core::config::MixOp;
use penna_core::generate::{Anchors, Proposal};
use penna_core::setup::{ControlProgress, Progress, Role, Step};
use penna_proto::{CurveKindName, Event, Op, Studio, StudioCurve};
use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// `load_check`: `Some(false)` skips it; `None` leaves it to the daemon, which runs it unless on battery.
pub fn run(controls: Vec<String>, load_check: Option<bool>, style: Style) -> Result<()> {
    let mut c = Client::find().ok_or_else(client::not_running)?;
    if !c.hello.can_write {
        bail!(c.hello.why.clone().unwrap_or_else(|| "pennad doesn't let this user change fans.".into()));
    }
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst)).context("installing the Ctrl-C handler")?;
    }
    let only = (!controls.is_empty()).then_some(controls);
    let started = c.call(Op::SetupStart { controls: only, load_check })?;
    if let Some(text) = started.as_ref().and_then(|r| r.get("text")).and_then(|t| t.as_str()) {
        println!("{text}.");
    }
    println!("You will hear the fans. Setup stops and hands every fan back if the CPU passes 85 °C while fans are measured, the");
    println!("GPU passes 80 °C, or a sensor fails to read. Ctrl-C does the same.\n");

    let live = std::io::stdout().is_terminal();
    let mut out = std::io::stdout().lock();
    let mut seen = Step::Idle;
    let mut last: Option<Progress> = None;
    let mut status_shown = false;
    let result = c.subscribe(|_, ev| {
        if stop.load(Ordering::SeqCst) {
            return Ok(false);
        }
        let Some(Event::Tick(t)) = ev else { return Ok(true) };
        let Some(p) = t.setup else { return Ok(true) };
        let p = *p;
        if live && status_shown {
            write!(out, "\r\x1b[2K")?;
            status_shown = false;
        }
        while seen < p.step {
            write!(out, "{}", step_lines(seen, last.as_ref().unwrap_or(&p), &p, style))?;
            seen = next(seen, &p);
        }
        if p.stopped.is_some() || p.step == Step::Review {
            last = Some(p);
            out.flush()?;
            return Ok(false);
        }
        if live {
            let say = p.waiting.clone().unwrap_or_else(|| p.say.clone());
            write!(out, "{}", style.dim(&format!("     {}  {}", clock(p.elapsed), truncate(&say, 96))))?;
            status_shown = true;
        }
        out.flush()?;
        last = Some(p);
        Ok(true)
    });
    if live && status_shown {
        write!(out, "\r\x1b[2K")?;
    }
    drop(out);
    if stop.load(Ordering::SeqCst) {
        println!("Stopped: every fan is back as it was found. Nothing was saved.");
        return Ok(());
    }
    result?;
    let p = last.ok_or_else(|| anyhow!("pennad stopped sending setup's progress"))?;
    if let Some(why) = &p.stopped {
        bail!("setup stopped: {why} Every fan is back as it was found.");
    }
    print!("{}", step_lines(Step::Review, &p, &p, style));
    let proposed = confirm_roles(&mut c, &p)?.or(p.proposed);
    curves(&mut c, proposed, style)
}

/// The step after `s` in this run: the load check only when it has one.
fn next(s: Step, p: &Progress) -> Step {
    p.steps.iter().map(|x| x.step).find(|x| *x > s).unwrap_or(Step::Review)
}

/// The lines for a step that has just ended, from what the run says now.
fn step_lines(step: Step, before: &Progress, now: &Progress, style: Style) -> String {
    let n = now.steps.iter().position(|s| s.step == step).map_or(0, |i| i + 1);
    let total = now.steps.len();
    let seconds = now.steps.iter().find(|s| s.step == step).map_or(0.0, |s| s.seconds);
    let head = format!("{n}/{total}  {:<12} {:>5}   ", step.name(), if seconds > 0.0 { clock(seconds) } else { String::new() });
    let pad = " ".repeat(head.chars().count());
    let ctls = &now.controls;
    let name = |c: &ControlProgress| c.header.clone().unwrap_or_else(|| c.label.clone());
    let mut lines: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    match step {
        Step::Idle => {
            let g: Vec<String> = before
                .guards
                .iter()
                .map(|g| format!("{} {}", g.label, g.celsius.map_or("—".to_owned(), |c| format!("{c:.0} °C"))))
                .collect();
            lines.push(g.join(" · "));
        }
        Step::WriteTest => {
            let ok = ctls.iter().filter(|c| c.write == Some(penna_core::setup::WriteResult::Ok)).count();
            lines.push(format!("{ok} of {} ok", ctls.len()));
            for c in ctls.iter().filter(|c| c.write.is_some_and(|w| w != penna_core::setup::WriteResult::Ok)) {
                notes.push(format!("{}: {}", c.label, c.write_detail.clone().unwrap_or_default()));
            }
        }
        Step::Pairing => {
            let mut found = Vec::new();
            for c in ctls.iter().filter(|c| c.paired == Some(true) && !c.key.as_str().starts_with("nvml/")) {
                found.push(format!("{} → {}", c.label, name(c)));
            }
            for chunk in found.chunks(3) {
                lines.push(chunk.join(", "));
            }
            for c in ctls.iter().filter(|c| c.paired == Some(false)) {
                notes.push(format!("{}: no fan turns on it. It stays with its firmware.", c.label));
            }
            for c in ctls.iter().filter(|c| c.write_detail.as_deref().is_some_and(|d| d.contains("isn't paired"))) {
                notes.push(format!("{}: {}", c.label, c.write_detail.clone().unwrap_or_default()));
            }
        }
        Step::Roles => {
            let mut by: BTreeMap<Role, Vec<String>> = BTreeMap::new();
            for c in ctls {
                if let Some(s) = &c.suggestion {
                    by.entry(s.role).or_default().push(format!("{}{}", name(c), if s.confirm { "?" } else { "" }));
                }
            }
            let parts: Vec<String> = by.iter().map(|(r, n)| format!("{}: {}", r.name(), n.join(", "))).collect();
            for chunk in parts.chunks(2) {
                lines.push(chunk.join(" · "));
            }
            for c in ctls {
                if let Some(s) = c.suggestion.as_ref().filter(|s| s.confirm || s.may_stop) {
                    notes.push(format!("{}: {}", name(c), s.reason));
                }
            }
        }
        Step::Calibration => {
            for c in ctls.iter().filter(|c| !c.table.is_empty()) {
                let top = c.table.last().map_or(String::new(), |t| format!("{} RPM at 100 %", thousands(t[1])));
                let what = match (c.stop, c.start, c.lowest) {
                    (Some(s), Some(st), _) => format!("stops below {s:.0} %, starts at {st:.0} %, {top}"),
                    (Some(s), None, _) if s <= 0.0 => format!("turns even at 0 %, {top}"),
                    (Some(s), None, _) => format!("stops below {s:.0} %, {top}"),
                    (None, _, Some(l)) => format!("measured down to {l:.0} %, {top}"),
                    _ => top,
                };
                lines.push(format!("{:<14} {what}", name(c)));
                if let Some(note) = &c.note {
                    notes.push(format!("{}: {note}", name(c)));
                }
            }
        }
        Step::LoadCheck => match now.load.as_ref().and_then(|l| l.result.as_ref()) {
            Some(r) => lines.push(penna_core::setup::load_words(r)),
            None => lines.push("didn't finish".into()),
        },
        Step::Review => {
            return format!("\nDone in {}. Every fan is back with its BIOS or driver; what setup measured is saved.\n", clock(now.elapsed));
        }
    }
    let mut s = String::new();
    for (i, l) in lines.iter().enumerate() {
        s.push_str(if i == 0 { &head } else { &pad });
        s.push_str(l);
        s.push('\n');
    }
    if lines.is_empty() {
        s.push_str(&head);
        s.push('\n');
    }
    for n in notes {
        s.push_str(&pad);
        s.push_str(&style.dim(&n));
        s.push('\n');
    }
    s
}

/// The roles setup suggested, confirmed by the user (§5.3). A dashed suggestion asks; the rest are taken as they are. Returns
/// the curves proposed for the confirmed roles, when the daemon proposed again.
fn confirm_roles(c: &mut Client, p: &Progress) -> Result<Option<Proposal>> {
    let interactive = std::io::stdin().is_terminal();
    let mut roles = BTreeMap::new();
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    for c in &p.controls {
        let Some(s) = &c.suggestion else { continue };
        let name = c.header.clone().unwrap_or_else(|| c.label.clone());
        let mut role = s.role;
        if s.confirm && interactive {
            println!("{name}: {}", s.reason);
            loop {
                print!("  {}? Enter to confirm, or type cpu, case, pump or chipset: ", role.name().to_lowercase());
                std::io::stdout().flush()?;
                let mut line = String::new();
                input.read_line(&mut line)?;
                let answer = line.trim();
                if answer.is_empty() {
                    break;
                }
                match Role::parse(answer) {
                    Some(r) if r != Role::Gpu && r != Role::Laptop => {
                        role = r;
                        break;
                    }
                    _ => println!("  {answer:?} isn't one of those."),
                }
            }
        }
        roles.insert(c.key.to_string(), role_id(role).to_owned());
    }
    if roles.is_empty() {
        return Ok(None);
    }
    if !interactive {
        println!("Roles are suggestions until confirmed: run penna setup in a terminal, or confirm them in the studio.");
        return Ok(None);
    }
    print!("Confirm these roles? [Y/n] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    if line.trim().eq_ignore_ascii_case("n") {
        println!("Not confirmed. They stay suggestions until you confirm them.");
        return Ok(None);
    }
    let r = c.call(Op::SetupRoles { roles })?;
    let text = r.as_ref().and_then(|r| r.get("text")).and_then(|t| t.as_str()).unwrap_or_default().to_owned();
    println!("{text}.");
    if !text.contains("measuring") {
        let state = c.call(Op::State)?.unwrap_or_default();
        return Ok(state.get("setup").and_then(|s| s.get("proposed")).and_then(|p| serde_json::from_value(p.clone()).ok()));
    }
    // A case fan on a pump header is measured below 50 % now, on this connection: stay until it is done.
    let mut done = None;
    let mut running = false;
    c.subscribe(|_, ev| {
        let Some(Event::Tick(t)) = ev else { return Ok(true) };
        match t.setup {
            Some(p) if p.stopped.is_some() || p.step == Step::Review => {
                if running {
                    done = Some(*p);
                }
                Ok(!running)
            }
            Some(_) => {
                running = true;
                Ok(true)
            }
            None => Ok(true),
        }
    })?;
    match done {
        Some(p) if p.stopped.is_none() => {
            for c in &p.controls {
                let name = c.header.clone().unwrap_or_else(|| c.label.clone());
                if let Some(stop) = c.stop {
                    let start = c.start.map_or(String::new(), |s| format!(", starts at {s:.0} %"));
                    println!("{name}: stops below {stop:.0} %{start}.");
                }
                if let Some(note) = &c.note {
                    println!("{name}: {note}");
                }
            }
            Ok(p.proposed)
        }
        Some(p) => {
            println!("Stopped: {}", p.stopped.unwrap_or_default());
            Ok(None)
        }
        None => Ok(None),
    }
}

/// The curves setup proposes (§5.5), a draft on this connection: shown in words, then saved, which switches to Balanced, or left,
/// which keeps every fan where it is.
fn curves(c: &mut Client, proposed: Option<Proposal>, style: Style) -> Result<()> {
    let Some(p) = proposed else {
        println!("Setup proposed no curves. Adopt fans in the studio: penna open.");
        return Ok(());
    };
    if let Some(why) = &p.refused {
        println!("{why}");
        return Ok(());
    }
    let studio: Studio = serde_json::from_value(c.call(Op::Studio)?.unwrap_or_default()).context("the studio's reply")?;
    println!("\nProposed curves, a draft: nothing changes until you save it.");
    print!("{}", curve_lines(&studio, &p));
    for n in &p.notes {
        println!("{}", style.dim(&format!("  {n}")));
    }
    if !std::io::stdin().is_terminal() {
        println!("The draft goes when this command ends: run penna setup in a terminal to save it.");
        return Ok(());
    }
    print!("Save these curves and switch to Balanced? [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    if line.trim().eq_ignore_ascii_case("y") {
        c.call(Op::DraftSave)?;
        println!("Saved. Penna runs every fan on Balanced; switch profiles in the popup, or with penna profile.");
    } else {
        c.call(Op::DraftDiscard)?;
        println!("Not saved: every fan stays with its BIOS or driver. What setup measured is kept; run penna setup again for curves.");
    }
    Ok(())
}

const PROFILES: [(&str, &str); 3] = [("quiet", "Quiet"), ("balanced", "Balanced"), ("render", "Render")];

/// One block per group of fans that share curves: who they are and what they follow, then a line per profile.
fn curve_lines(studio: &Studio, p: &Proposal) -> String {
    let assign = |profile: &str| studio.profiles.iter().find(|x| x.id == profile).map(|x| x.assign.clone()).unwrap_or_default();
    let balanced = assign("balanced");
    let curve = |id: &str| studio.curves.iter().find(|c| c.id == id);
    let label = |k: &penna_core::Key| studio.controls.iter().find(|c| &c.key == k).map_or_else(|| k.to_string(), |c| c.label.clone());
    // Fans grouped by the curve they follow in Balanced, in the order setup made the curves.
    let mut groups: Vec<(String, Vec<penna_core::Key>)> = Vec::new();
    for id in &p.curves {
        let fans: Vec<penna_core::Key> = balanced.iter().filter(|(_, c)| *c == id).map(|(k, _)| k.clone()).collect();
        if !fans.is_empty() {
            groups.push((id.clone(), fans));
        }
    }
    let mut s = String::new();
    for (id, fans) in groups {
        let Some(b) = curve(&id) else { continue };
        let names: Vec<String> = fans.iter().map(label).collect();
        let role = b.label.as_deref().and_then(|l| l.split(" · ").next()).unwrap_or(&id).to_owned();
        let follows = match (&b.mix, &b.source) {
            (Some(_), _) => "the larger of a CPU and a GPU curve".to_owned(),
            (None, Some(src)) => format!("on {}{}", b.source_label.clone().unwrap_or_else(|| src.to_string()), anchors(p.anchors.get(src))),
            _ => String::new(),
        };
        s.push_str(&format!(
            "  {} · {role}{}\n",
            names.join(", "),
            if follows.is_empty() { String::new() } else { format!(", {follows}") }
        ));
        let per: Vec<(&str, Option<&StudioCurve>)> = PROFILES
            .iter()
            .map(|(pid, name)| (*name, fans.first().and_then(|k| assign(pid).get(k).cloned()).and_then(|c| curve(&c))))
            .collect();
        if per.iter().all(|(_, c)| c.map(|c| &c.id) == Some(&b.id)) {
            s.push_str(&format!("    {:<10} {}\n", "All three", describe(b, studio)));
        } else if per.iter().all(|(_, c)| c.is_some_and(|c| c.kind == CurveKindName::Flat)) {
            let flat: Vec<String> =
                per.iter().map(|(n, c)| format!("{n} {}", c.and_then(|c| c.duty).map_or("—".into(), |d| format!("{d:.0} %")))).collect();
            s.push_str(&format!("    {}\n", flat.join(" · ")));
        } else {
            for (name, c) in per {
                s.push_str(&format!("    {name:<10} {}\n", c.map_or("its firmware".to_owned(), |c| describe(c, studio))));
            }
        }
    }
    s
}

fn anchors(a: Option<&Anchors>) -> String {
    let Some(a) = a else { return String::new() };
    let mut parts = vec![format!("idle {:.0} °C", a.idle)];
    if let Some(t) = a.target {
        parts.push(format!("target {t:.0} °C"));
    }
    if let Some(l) = a.limit {
        parts.push(format!("limit {l:.0} °C"));
    }
    format!(" ({})", parts.join(", "))
}

/// A curve in words: where it starts, its middle and its top.
fn describe(c: &StudioCurve, studio: &Studio) -> String {
    if let Some(d) = c.duty {
        return format!("{d:.0} %");
    }
    if let Some(m) = &c.mix {
        let parts: Vec<(String, Vec<[f64; 2]>)> =
            m.of.iter()
                .filter_map(|id| studio.curves.iter().find(|x| &x.id == id))
                .filter_map(|x| Some((x.label.as_deref()?.rsplit(" · ").next()?.to_owned(), x.points.clone()?)))
                .collect();
        let op = if m.op == MixOp::Max { "" } else { "mixed " };
        let from: Vec<String> = parts.iter().map(|(n, p)| format!("{:.0} °C ({n})", p[0][0])).collect();
        let top: Vec<String> = parts.iter().map(|(n, p)| format!("{:.0} °C ({n})", p[p.len() - 1][0])).collect();
        let duty = parts.first().map_or(0.0, |(_, p)| p[p.len() - 1][1]);
        let floor = parts.first().map_or(0.0, |(_, p)| p[0][1]);
        return format!("{op}{floor:.0} % until {}, up to {duty:.0} % at {}", from.join(" or "), top.join(" or "));
    }
    let Some(pts) = &c.points else { return String::new() };
    let gpu = c.source.as_ref().is_some_and(|s| s.as_str().starts_with("nvml/"));
    let mut words = Vec::new();
    let mut rest: &[[f64; 2]] = pts.as_slice();
    if let [[_, d], second, ..] = pts.as_slice()
        && *d == 0.0
    {
        words.push(if gpu {
            format!("the driver's curve below {:.0} °C", second[0])
        } else {
            format!("rests below {:.0} °C", second[0])
        });
        rest = &pts[1..];
    }
    if let Some(first) = rest.first() {
        words.push(if words.is_empty() {
            format!("{:.0} % until {:.0} °C", first[1], first[0])
        } else {
            format!("{:.0} % at {:.0} °C", first[1], first[0])
        });
    }
    if rest.len() > 2 {
        let mid = rest[rest.len() / 2];
        words.push(format!("{:.0} % at {:.0} °C", mid[1], mid[0]));
    }
    if rest.len() > 1 {
        let last = rest[rest.len() - 1];
        words.push(format!("{:.0} % at {:.0} °C", last[1], last[0]));
    }
    words.join(", ")
}

fn role_id(r: Role) -> &'static str {
    match r {
        Role::Cpu => "cpu",
        Role::Gpu => "gpu",
        Role::Pump => "pump",
        Role::Chipset => "chipset",
        Role::Laptop => "laptop",
        Role::Case => "case",
    }
}

fn clock(s: f64) -> String {
    let s = s.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn thousands(v: f64) -> String {
    let n = ((v / 10.0).round() * 10.0) as u64;
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_owned() } else { format!("{}…", s.chars().take(n - 1).collect::<String>()) }
}
