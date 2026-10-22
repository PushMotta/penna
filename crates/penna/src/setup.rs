//! `penna setup` (DESIGN.md §5, §11.3): the same run as the studio's setup view, in a terminal. It holds its own connection, so
//! Ctrl-C, or a closed terminal, stops setup and every fan goes back as it was found. One line per step as it ends, a live line
//! while it runs, then the roles to confirm.
use crate::client::{self, Client};
use crate::render::Style;
use anyhow::{Context, Result, anyhow, bail};
use penna_core::setup::{ControlProgress, Progress, Role, Step};
use penna_proto::{Event, Op};
use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub fn run(controls: Vec<String>, style: Style) -> Result<()> {
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
    let started = c.call(Op::SetupStart { controls: only, load_check: None })?;
    if let Some(text) = started.as_ref().and_then(|r| r.get("text")).and_then(|t| t.as_str()) {
        println!("{text}.");
    }
    println!("You will hear the fans. Setup stops and hands every fan back if the CPU passes 85 °C, the GPU passes 80 °C, or a");
    println!("sensor fails to read. Ctrl-C does the same.\n");

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
            seen = next(seen);
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
    confirm_roles(&p)
}

fn next(s: Step) -> Step {
    match s {
        Step::Idle => Step::WriteTest,
        Step::WriteTest => Step::Pairing,
        Step::Pairing => Step::Roles,
        Step::Roles => Step::Calibration,
        Step::Calibration | Step::Review => Step::Review,
    }
}

/// The lines for a step that has just ended, from what the run says now.
fn step_lines(step: Step, before: &Progress, now: &Progress, style: Style) -> String {
    let n = match step {
        Step::Idle => 1,
        Step::WriteTest => 2,
        Step::Pairing => 3,
        Step::Roles => 4,
        Step::Calibration => 5,
        Step::Review => 6,
    };
    let seconds = now.steps.iter().find(|s| s.step == step).map_or(0.0, |s| s.seconds);
    let head = format!("{n}/6  {:<12} {:>5}   ", step.name(), if seconds > 0.0 { clock(seconds) } else { String::new() });
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
                    (Some(s), None, _) => format!("stops below {s:.0} %, {top}"),
                    (None, _, Some(l)) => format!("measured down to {l:.0} %, {top}"),
                    _ => top,
                };
                lines.push(format!("{:<14} {what}", name(c)));
            }
        }
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

/// The roles setup suggested, confirmed by the user (§5.3). A dashed suggestion asks; the rest are taken as they are.
fn confirm_roles(p: &Progress) -> Result<()> {
    let interactive = std::io::stdin().is_terminal();
    let mut roles = BTreeMap::new();
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    for c in &p.controls {
        let Some(s) = &c.suggestion else { continue };
        let name = c.header.clone().unwrap_or_else(|| c.label.clone());
        let mut role = s.role;
        if s.confirm && interactive {
            let other = if role == Role::Pump { Role::Case } else { Role::Pump };
            print!(
                "{name}: {} ({}) or {} ({})? [{}] ",
                role.name().to_lowercase(),
                key(role),
                other.name().to_lowercase(),
                key(other),
                key(role)
            );
            std::io::stdout().flush()?;
            let mut line = String::new();
            input.read_line(&mut line)?;
            if line.trim().eq_ignore_ascii_case(key(other)) {
                role = other;
            }
        }
        roles.insert(c.key.to_string(), role_id(role).to_owned());
    }
    if roles.is_empty() {
        return Ok(());
    }
    if !interactive {
        println!("Roles are suggestions until confirmed: run penna setup in a terminal, or confirm them in the studio.");
        return Ok(());
    }
    print!("Confirm these roles? [Y/n] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    if line.trim().eq_ignore_ascii_case("n") {
        println!("Not confirmed. They stay suggestions until you confirm them.");
        return Ok(());
    }
    let mut c = Client::find().ok_or_else(client::not_running)?;
    let r = c.call(Op::SetupRoles { roles })?;
    if let Some(text) = r.as_ref().and_then(|r| r.get("text")).and_then(|t| t.as_str()) {
        println!("{text}.");
    }
    println!("Generated curves arrive with M8; until then, adopt fans in the studio (penna open).");
    Ok(())
}

fn key(r: Role) -> &'static str {
    match r {
        Role::Pump => "p",
        _ => "c",
    }
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
