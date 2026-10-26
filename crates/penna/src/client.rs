//! Talking to `pennad` (DESIGN.md §7.3): find its socket, read its hello, send requests and wait for their replies, and stream
//! what a subscriber gets.
use anyhow::{Context, Result, anyhow, bail};
use penna_proto::{Event, Hello, Op, Reply, Request};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(5);
/// The longest line read from a daemon. Its ticks stay far under a protocol line (§7.4); a daemon that sends more is broken or
/// isn't pennad.
const LINE_MAX: u64 = 4 * penna_proto::MAX_LINE as u64;

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    pub hello: Hello,
    pub path: PathBuf,
    next_id: u64,
}

/// What a person sees when no daemon answers.
pub fn not_running() -> anyhow::Error {
    let places: Vec<String> = penna_proto::socket_candidates().iter().map(|p| p.display().to_string()).collect();
    anyhow!(
        "Penna's daemon isn't running: nothing answers at {}. Start it with `sudo systemctl start pennad`, or try the mock desk: \
         `pennad --mock render-burst --forever`.",
        places.join(" or ")
    )
}

impl Client {
    /// The first daemon that answers, in the order of §7.3, or `None`.
    pub fn find() -> Option<Client> {
        penna_proto::socket_candidates().into_iter().find_map(|p| Client::connect(&p).ok())
    }

    pub fn connect(path: &Path) -> Result<Client> {
        let stream = UnixStream::connect(path).with_context(|| format!("connecting to {}", path.display()))?;
        stream.set_read_timeout(Some(TIMEOUT))?;
        let writer = stream.try_clone()?;
        let mut reader = BufReader::new(stream);
        let hello = match read_event(&mut reader)? {
            Some(Event::Hello(h)) => h,
            Some(_) => bail!("{} did not say hello first", path.display()),
            None => bail!("{} closed before saying hello", path.display()),
        };
        if hello.v != penna_proto::VERSION {
            bail!("pennad at {} speaks protocol {}, this penna speaks {}", path.display(), hello.v, penna_proto::VERSION);
        }
        Ok(Client { reader, writer, hello, path: path.to_path_buf(), next_id: 1 })
    }

    /// One request and its reply. Lines that are not this reply (ticks, events) are skipped.
    pub fn call(&mut self, op: Op) -> Result<Option<Value>> {
        let id = self.next_id;
        self.next_id += 1;
        let line = serde_json::to_string(&Request { id: Some(Value::from(id)), op })?;
        writeln!(self.writer, "{line}").context("sending to pennad")?;
        loop {
            let Some(value) = read_value(&mut self.reader).context("waiting for pennad")? else {
                bail!("pennad closed the connection");
            };
            if value.get("ev").is_some() {
                continue;
            }
            let reply: Reply = serde_json::from_value(value)?;
            if reply.id != Some(Value::from(id)) {
                continue;
            }
            return if reply.ok { Ok(reply.result) } else { Err(anyhow!(reply.error.unwrap_or_else(|| "pennad refused".into()))) };
        }
    }

    /// Subscribes, then hands every line after the reply to `each` (raw, and parsed when it is an event) until it says stop. The
    /// connection stays open, unsubscribed: what it holds, such as setup's draft, stays with it.
    pub fn subscribe(&mut self, mut each: impl FnMut(&str, Option<Event>) -> Result<bool>) -> Result<()> {
        self.call(Op::Subscribe)?;
        self.reader.get_ref().set_read_timeout(None)?;
        loop {
            let Some(value) = read_value(&mut self.reader).context("reading from pennad")? else {
                bail!("pennad closed the connection");
            };
            let line = value.to_string();
            let ev = serde_json::from_value::<Event>(value).ok();
            if !each(&line, ev)? {
                self.reader.get_ref().set_read_timeout(Some(TIMEOUT))?;
                self.call(Op::Unsubscribe)?;
                return Ok(());
            }
        }
    }

    /// Where the daemon is, said the way a person would type it.
    pub fn via(&self) -> String {
        let p = self.path.display().to_string();
        match std::env::var("XDG_RUNTIME_DIR") {
            Ok(d) if !d.is_empty() && p.starts_with(&d) => p.replacen(&d, "$XDG_RUNTIME_DIR", 1),
            _ => p,
        }
    }
}

fn read_event(reader: &mut BufReader<UnixStream>) -> Result<Option<Event>> {
    match read_value(reader)? {
        Some(v) => Ok(Some(serde_json::from_value(v).context("pennad's first line")?)),
        None => Ok(None),
    }
}

/// One line from the daemon, parsed, with every string made safe to print ([`printable`]). `None` when it closed.
fn read_value(reader: &mut BufReader<UnixStream>) -> Result<Option<Value>> {
    use std::io::Read;
    let mut buf = String::new();
    let n = reader.by_ref().take(LINE_MAX + 1).read_line(&mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    if n as u64 > LINE_MAX {
        bail!("pennad sent a line over {} MiB", LINE_MAX >> 20);
    }
    let mut value: Value = serde_json::from_str(&buf).context("pennad sent a line that isn't JSON")?;
    printable(&mut value);
    Ok(Some(value))
}

/// Every string in `v` with what a terminal would act on replaced by `�`: escape sequences, other controls, and what reorders
/// the text around it (`penna_core::config::hidden`). Names come from any member of the penna group, and the person reading
/// them may be anyone, root under sudo included. Since 0.10.6 pennad refuses such names; an older one, or another program on
/// the socket, may not.
pub fn printable(v: &mut Value) {
    match v {
        Value::String(s) if s.chars().any(penna_core::config::hidden) => {
            *s = s.chars().map(|c| if penna_core::config::hidden(c) { '\u{FFFD}' } else { c }).collect();
        }
        Value::Array(items) => items.iter_mut().for_each(printable),
        Value::Object(map) => map.values_mut().for_each(printable),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a terminal would act on never reaches one: an older pennad, or another program on the socket, may still send it.
    #[test]
    fn a_name_from_the_daemon_is_printed_as_characters() {
        let mut v = serde_json::json!({
            "label": "Rear \u{1b}]52;c;cm0gLXJm\u{7} fan",
            "nested": [{"reason": "two\nlines \u{9b}31m \u{202e}desrever"}],
            "fine": "CPU cooler · 140 mm",
            "n": 3
        });
        printable(&mut v);
        assert_eq!(v["label"], "Rear \u{FFFD}]52;c;cm0gLXJm\u{FFFD} fan");
        assert_eq!(v["nested"][0]["reason"], "two\u{FFFD}lines \u{FFFD}31m \u{FFFD}desrever");
        assert_eq!(v["fine"], "CPU cooler · 140 mm");
        assert_eq!(v["n"], 3);
    }
}
