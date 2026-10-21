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
            let mut buf = String::new();
            if self.reader.read_line(&mut buf).context("waiting for pennad")? == 0 {
                bail!("pennad closed the connection");
            }
            let value: Value = serde_json::from_str(&buf).context("pennad sent a line that isn't JSON")?;
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

    /// Subscribes, then hands every line after the reply to `each` (raw, and parsed when it is an event) until it says stop.
    pub fn subscribe(mut self, mut each: impl FnMut(&str, Option<Event>) -> Result<bool>) -> Result<()> {
        self.call(Op::Subscribe)?;
        self.reader.get_ref().set_read_timeout(None)?;
        loop {
            let mut buf = String::new();
            if self.reader.read_line(&mut buf).context("reading from pennad")? == 0 {
                bail!("pennad closed the connection");
            }
            let line = buf.trim_end();
            let ev = serde_json::from_str::<Event>(line).ok();
            if !each(line, ev)? {
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
    let mut buf = String::new();
    if reader.read_line(&mut buf)? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&buf).context("pennad's first line")?))
}
