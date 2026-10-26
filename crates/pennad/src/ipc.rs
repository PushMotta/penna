//! The socket surfaces talk to (DESIGN.md §7.1, §7.3, §7.4). It runs on its own thread with a current-thread tokio runtime, and
//! never waits on the control loop, nor the loop on it:
//!
//! * the loop publishes each tick through a `watch` channel: a subscriber that falls behind skips ticks, it never queues them;
//! * transitions go out through a `broadcast` channel; a subscriber that lags behind it is dropped;
//! * requests that change fans go to the loop through a bounded queue it drains between ticks, each with a oneshot reply;
//! * a client that stops reading is dropped after [`WRITE_TIMEOUT`] rather than holding anything up;
//! * no uid holds more than [`PER_UID_MAX`] connections, nor all of them more than [`CONNECTIONS_MAX`], so no local user can
//!   take the descriptors `pennad` needs to drive and hand back fans (§6). A connection that asks nothing in [`IDLE`] is closed.
use crate::auth::{self, Peer, Policy};
use anyhow::{Context, Result, bail};
use penna_proto::{Event, Hello, History, Op, Reply, Request};
use serde_json::Value;
use std::collections::HashMap;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

/// A client that cannot take a line for this long is dropped.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a request may wait for the control loop.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// Requests waiting for the loop. More than this and the daemon says it is busy.
pub const QUEUE: usize = 64;
/// Transitions a subscriber may fall behind by before it is dropped.
pub const EVENT_BACKLOG: usize = 256;
/// Connections at once: far more than a desk's shells and CLIs, far fewer than the daemon's descriptors.
pub const CONNECTIONS_MAX: usize = 256;
/// Connections at once from one uid other than root: a shell's service and studio, and many CLIs.
pub const PER_UID_MAX: usize = 32;
/// A connection that has sent nothing this long after its hello is closed. Once it has asked something it stays, as the
/// studio's does while it holds a draft.
pub const IDLE: Duration = Duration::from_secs(30);
/// The longest request. The largest a surface sends, a curve with all its points, is a few kilobytes; a connection's waiting
/// lines then cost the daemon a few hundred kilobytes at most, about 10 MiB for all a uid may hold.
pub const REQUEST_MAX: usize = 64 << 10;
/// At most one line in the journal this often about refused connections or failing accepts.
const QUIET: Duration = Duration::from_secs(10);

/// What the loop publishes after every tick, and after every request that changed something.
pub struct Published {
    /// The tick body (`state`'s result).
    pub body: Arc<str>,
    /// The same, as a `tick` event line.
    pub tick_line: Arc<str>,
    pub history: Arc<History>,
    /// The active configuration as JSON (`config`'s result).
    pub config: Arc<str>,
}

impl Published {
    pub fn new(tick: &penna_proto::Tick, history: History, config: &penna_core::config::Config) -> Published {
        let body = serde_json::to_string(tick).expect("ticks always serialize");
        let tick_line = format!("{{\"ev\":\"tick\",{}", &body[1..]);
        Published {
            body: body.into(),
            tick_line: tick_line.into(),
            history: Arc::new(history),
            config: serde_json::to_string(config).expect("configurations always serialize").into(),
        }
    }
}

/// A request for the control loop, with who asked and on which connection (a draft belongs to its connection, §7.4).
pub struct Job {
    pub op: Op,
    pub uid: u32,
    pub conn: u64,
    pub reply: oneshot::Sender<Result<Option<Value>, String>>,
}

pub struct Setup {
    pub hello: Hello,
    pub policy: Policy,
    pub published: watch::Receiver<Arc<Published>>,
    pub events: broadcast::Sender<Arc<str>>,
    pub jobs: SyncSender<Job>,
    /// Every connection's number when it closes.
    pub closed: std::sync::mpsc::Sender<u64>,
    /// [`IDLE`], but for tests.
    pub idle: Duration,
}

pub struct Ipc {
    thread: std::thread::JoinHandle<()>,
    path: PathBuf,
}

/// Binds the socket, refusing to take over one another daemon still answers on, then serves it on its own thread. The server
/// stops when the loop drops its `watch` sender.
pub fn start(path: &Path, setup: Setup) -> Result<Ipc> {
    let listener = bind(path)?;
    let path = path.to_path_buf();
    let thread = std::thread::Builder::new()
        .name("pennad-ipc".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a tokio runtime");
            rt.block_on(serve(listener, setup));
        })
        .context("starting the socket thread")?;
    Ok(Ipc { thread, path })
}

impl Ipc {
    /// Waits for the server to finish (its `watch` sender must be gone), then removes the socket.
    pub fn stop(self) {
        let _ = self.thread.join();
        let _ = std::fs::remove_file(&self.path);
    }
}

fn bind(path: &Path) -> Result<std::os::unix::net::UnixListener> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {} (try --socket)", dir.display()))?;
    }
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() {
            bail!("{} exists and is not a socket; move it away, or pass --socket", path.display());
        }
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            bail!("another pennad is already listening on {}; stop it first, or pass --socket", path.display());
        }
        // Nobody answers: a daemon before this one died without cleaning up.
        std::fs::remove_file(path).with_context(|| format!("removing the stale socket {}", path.display()))?;
    }
    let listener = std::os::unix::net::UnixListener::bind(path).with_context(|| format!("listening on {}", path.display()))?;
    // World-connectable (§7.4): every connection's credentials decide what it may do.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666)).with_context(|| format!("opening up {}", path.display()))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

async fn serve(listener: std::os::unix::net::UnixListener, setup: Setup) {
    let listener = match tokio::net::UnixListener::from_std(listener) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("pennad: the socket stopped: {e}");
            return;
        }
    };
    let setup = Arc::new(setup);
    let slots = Slots::default();
    let mut stopping = setup.published.clone();
    let mut next: u64 = 0;
    let mut quiet = Quiet::default();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let uid = stream.peer_cred().map(|c| c.uid()).unwrap_or(u32::MAX);
                    match slots.take(uid) {
                        Some(slot) => {
                            next += 1;
                            tokio::spawn(connection(stream, setup.clone(), next, slot));
                        }
                        // Closed at once: it never gets a hello, so a client sees the connection end.
                        None => quiet.say(format_args!("pennad: refused a connection from uid {uid}: it already holds too many")),
                    }
                }
                Err(e) => {
                    // Out of descriptors, the listener stays ready and fails again at once: wait rather than spin.
                    quiet.say(format_args!("pennad: accepting a connection failed: {e}"));
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            changed = stopping.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}

/// Who holds how many connections. A [`Slot`] gives its place back when its connection ends.
#[derive(Clone, Default)]
struct Slots(Arc<Mutex<HashMap<u32, usize>>>);

struct Slot {
    slots: Slots,
    uid: u32,
}

impl Slots {
    fn take(&self, uid: u32) -> Option<Slot> {
        let mut held = self.0.lock().expect("slots");
        let total: usize = held.values().sum();
        let mine = held.get(&uid).copied().unwrap_or(0);
        if total >= CONNECTIONS_MAX || (uid != 0 && mine >= PER_UID_MAX) {
            return None;
        }
        *held.entry(uid).or_default() += 1;
        Some(Slot { slots: self.clone(), uid })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut held = self.slots.0.lock().expect("slots");
        if let Some(n) = held.get_mut(&self.uid) {
            *n -= 1;
            if *n == 0 {
                held.remove(&self.uid);
            }
        }
    }
}

/// A line for the journal at most every [`QUIET`], with how many like it were left out: a flood of connections can't flood
/// the journal too, or hide what matters in it.
#[derive(Default)]
struct Quiet {
    last: Option<Instant>,
    left_out: u64,
}

/// Clients dropped for not keeping up: any local user can make as many as they like, one after another.
static DROPPED: Mutex<Quiet> = Mutex::new(Quiet::new());

impl Quiet {
    const fn new() -> Quiet {
        Quiet { last: None, left_out: 0 }
    }

    fn say(&mut self, line: std::fmt::Arguments<'_>) {
        if self.last.is_some_and(|t| t.elapsed() < QUIET) {
            self.left_out += 1;
            return;
        }
        match std::mem::take(&mut self.left_out) {
            0 => eprintln!("{line}"),
            n => eprintln!("{line} ({n} more like it left out)"),
        }
        self.last = Some(Instant::now());
    }
}

async fn connection(stream: UnixStream, setup: Arc<Setup>, conn: u64, slot: Slot) {
    talk(stream, &setup, conn, Arc::new(slot)).await;
    // The loop drops this connection's draft, if it has one.
    let _ = setup.closed.send(conn);
}

/// Serves one connection. Its slot is held by both halves of the socket, here and in [`read_lines`], so it is given back only
/// once the daemon holds no descriptor for it.
async fn talk(stream: UnixStream, setup: &Setup, conn: u64, slot: Arc<Slot>) {
    let peer = match stream.peer_cred() {
        Ok(c) => Peer {
            uid: c.uid(),
            gid: c.gid(),
            groups: auth::peer_groups(std::os::fd::AsFd::as_fd(&stream)),
            listed: auth::listed("penna", c.uid()),
        },
        Err(e) => {
            eprintln!("pennad: a client without credentials ({e}); closed");
            return;
        }
    };
    // The group is looked up per connection too: installing Penna can create it after this daemon started.
    let decision = auth::decide(&peer, &Policy { penna_gid: auth::penna_gid(), ..setup.policy.clone() });
    let (rd, mut wr) = stream.into_split();
    let mut lines = read_lines(rd, slot.clone());
    let mut published = setup.published.clone();
    let mut events: Option<broadcast::Receiver<Arc<str>>> = None;

    let hello = Event::Hello(Hello { can_write: decision.is_ok(), why: decision.clone().err(), ..setup.hello.clone() });
    if send(&mut wr, &hello.line(), peer.uid).await.is_err() {
        return;
    }
    let mut asked = false;
    let idle = tokio::time::sleep(setup.idle);
    tokio::pin!(idle);
    loop {
        tokio::select! {
            () = &mut idle, if !asked => return,
            line = lines.rx.recv() => {
                let Some(line) = line else { return };
                let line = match line {
                    Ok(l) => l,
                    Err(reply) => {
                        let _ = send(&mut wr, &reply.line(), peer.uid).await;
                        return;
                    }
                };
                if line.trim().is_empty() {
                    continue;
                }
                let req = match Request::parse(&line) {
                    Ok(r) => r,
                    Err(reply) => {
                        if send(&mut wr, &reply.line(), peer.uid).await.is_err() { return }
                        continue;
                    }
                };
                // Only a request counts: a blank line, or one that isn't a request, asks nothing.
                asked = true;
                let id = req.id.clone();
                let out: Vec<String> = match req.op {
                    Op::State => vec![ok_raw(&id, &published.borrow().body)],
                    Op::Config => vec![ok_raw(&id, &published.borrow().config)],
                    Op::Subscribe => {
                        events = Some(setup.events.subscribe());
                        let p = published.borrow_and_update().clone();
                        let history = Event::History((*p.history).clone()).line();
                        vec![Reply::ok(id, None).line(), history, p.tick_line.to_string()]
                    }
                    Op::Unsubscribe => {
                        events = None;
                        vec![Reply::ok(id, None).line()]
                    }
                    op if op.writes() => vec![write(setup, &decision, op, id, peer.uid, conn).await.line()],
                    op => vec![ask(setup, op, id, peer.uid, conn).await.line()],
                };
                for l in out {
                    if send(&mut wr, &l, peer.uid).await.is_err() { return }
                }
            }
            changed = published.changed(), if events.is_some() => {
                if changed.is_err() {
                    return;
                }
                let line = published.borrow_and_update().tick_line.clone();
                if send(&mut wr, &line, peer.uid).await.is_err() { return }
            }
            ev = async { events.as_mut().expect("guarded").recv().await }, if events.is_some() => match ev {
                Ok(line) => {
                    if send(&mut wr, &line, peer.uid).await.is_err() { return }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    DROPPED.lock().expect("dropped").say(format_args!("pennad: dropped a client (uid {}) that fell {n} events behind", peer.uid));
                    return;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
        }
    }
}

/// A request that changes fans: checked against the peer, then carried out by the control loop.
async fn write(setup: &Setup, decision: &Result<(), String>, op: Op, id: Option<Value>, uid: u32, conn: u64) -> Reply {
    if let Err(why) = decision {
        return Reply::error(id, why.clone());
    }
    ask(setup, op, id, uid, conn).await
}

/// A request the control loop answers.
async fn ask(setup: &Setup, op: Op, id: Option<Value>, uid: u32, conn: u64) -> Reply {
    let (tx, rx) = oneshot::channel();
    match setup.jobs.try_send(Job { op, uid, conn, reply: tx }) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => return Reply::error(id, "Penna is busy with other requests; try again in a moment."),
        Err(TrySendError::Disconnected(_)) => return Reply::error(id, "Penna is shutting down."),
    }
    match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
        Ok(Ok(Ok(result))) => Reply::ok(id, result),
        Ok(Ok(Err(e))) => Reply::error(id, e),
        Ok(Err(_)) => Reply::error(id, "Penna stopped before it could answer."),
        Err(_) => Reply::error(id, "Penna did not answer in time; the request may still be applied."),
    }
}

/// `{"id":…,"ok":true,"result":<raw>}` without parsing the result back.
fn ok_raw(id: &Option<Value>, raw: &str) -> String {
    let id = serde_json::to_string(id).expect("ids serialize");
    format!("{{\"id\":{id},\"ok\":true,\"result\":{raw}}}")
}

async fn send(wr: &mut OwnedWriteHalf, line: &str, uid: u32) -> Result<(), ()> {
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    match tokio::time::timeout(WRITE_TIMEOUT, wr.write_all(&bytes)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(()),
        Err(_) => {
            DROPPED.lock().expect("dropped").say(format_args!("pennad: dropped a client (uid {uid}) that stopped reading"));
            Err(())
        }
    }
}

/// A client's lines, read on their own task. Dropping it stops the task, which holds the read half of the socket: a client that
/// keeps its end open and sends nothing can't keep the task, nor the descriptor, past its connection.
struct Lines {
    rx: mpsc::Receiver<Result<String, Reply>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Lines {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Lines from a client on their own task, so reading is never cancelled half way. A line over [`REQUEST_MAX`] ends the
/// connection with a reply that says why.
fn read_lines(rd: tokio::net::unix::OwnedReadHalf, slot: Arc<Slot>) -> Lines {
    // A connection's requests are answered one at a time, so two waiting is enough; more wait in the socket, not in pennad.
    let (tx, rx) = mpsc::channel(2);
    let task = tokio::spawn(async move {
        let _slot = slot;
        let mut reader = BufReader::new(rd);
        loop {
            let mut line = String::new();
            let n = match (&mut reader).take(REQUEST_MAX as u64 + 1).read_line(&mut line).await {
                Ok(n) => n,
                Err(e) => {
                    let _ = tx.send(Err(Reply::error(None, format!("That line couldn't be read: {e}.")))).await;
                    return;
                }
            };
            if n == 0 {
                return;
            }
            if line.len() > REQUEST_MAX {
                let _ = tx.send(Err(Reply::error(None, format!("A request is at most {REQUEST_MAX} bytes; closing.")))).await;
                return;
            }
            // Reading grows a line by doubling: a waiting line holds what it says, not twice that.
            line.shrink_to_fit();
            if tx.send(Ok(line)).await.is_err() {
                return;
            }
        }
    });
    Lines { rx, task }
}
