//! Sleep (DESIGN.md §6.5). As the kernel starts to suspend, the hardware stops answering as it should: on the desk the fan chip
//! refused writes, and handing a fan back hung the loop until the watchdog killed it. So `pennad` holds a logind delay lock.
//! When sleep is asked for, logind waits until the loop is between ticks; the loop leaves the hardware alone and lets the lock
//! go, and on waking it writes every fan it drives again ([`penna_core::control::LoopState::woke`]).
//!
//! logind is watched on a thread of its own with a current-thread tokio runtime, as the socket is. Without it, the loop runs
//! as before: read-back catches what sleep changed (§6.4).
use futures_lite::StreamExt;
use std::os::fd::OwnedFd;
use std::sync::mpsc;

/// What logind is told holds the lock, as `systemd-inhibit --list` shows it.
const WHY: &str = "Leaves the fans alone while the machine goes to sleep";
/// Loop time asleep without word from logind before the loop drives the fans again. Time spent asleep doesn't count: the loop
/// is frozen then, and its clock stops.
pub const GIVE_UP: f64 = 30.0;

enum Notice {
    /// Sleep was asked for. The lock comes with it: dropping it, between ticks, lets the machine sleep.
    Going(Option<OwnedFd>),
    /// Awake again, or the sleep failed.
    Woke,
    Trouble(String),
}

enum Source {
    /// A mock desk or a test tree: nothing sleeps.
    Off,
    Logind(mpsc::Receiver<Notice>),
    /// Test only: asleep from this tick to that one.
    Script {
        from: u64,
        to: u64,
    },
}

/// What [`Sleep::poll`] saw.
#[derive(Debug, PartialEq)]
pub enum Change {
    Going,
    Woke,
    /// No word from logind for [`GIVE_UP`] seconds.
    GaveUp,
    Trouble(String),
}

pub struct Sleep {
    source: Source,
    /// Loop time when the loop last left the hardware alone, while it does.
    since: Option<f64>,
}

impl Sleep {
    pub fn off() -> Sleep {
        Sleep { source: Source::Off, since: None }
    }

    /// Watches logind for sleep on this machine.
    pub fn watch() -> Sleep {
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("pennad-sleep".into()).spawn(move || {
            let why = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt.block_on(watch(&tx)),
                Err(e) => e.into(),
            };
            let _ = tx.send(Notice::Trouble(format!("{why:#}")));
        });
        match spawned {
            Ok(_) => Sleep { source: Source::Logind(rx), since: None },
            Err(_) => Sleep::off(),
        }
    }

    pub fn script(from: u64, to: u64) -> Sleep {
        Sleep { source: Source::Script { from, to }, since: None }
    }

    /// The loop leaves the hardware alone.
    pub fn asleep(&self) -> bool {
        self.since.is_some()
    }

    /// Takes what logind said. Called only between ticks, where letting the lock go is safe: the loop is touching nothing.
    pub fn poll(&mut self, ticks: u64, now: f64) -> Vec<Change> {
        let mut changes = Vec::new();
        match &self.source {
            Source::Off => {}
            Source::Logind(rx) => {
                for notice in rx.try_iter() {
                    match notice {
                        // The lock goes when this arm ends.
                        Notice::Going(_lock) if self.since.is_none() => {
                            self.since = Some(now);
                            changes.push(Change::Going);
                        }
                        Notice::Going(_) => {}
                        Notice::Woke if self.since.take().is_some() => changes.push(Change::Woke),
                        Notice::Woke => {}
                        Notice::Trouble(why) => changes.push(Change::Trouble(why)),
                    }
                }
            }
            Source::Script { from, to } => {
                if ticks >= *from && ticks < *to && self.since.is_none() {
                    self.since = Some(now);
                    changes.push(Change::Going);
                } else if ticks >= *to && self.since.take().is_some() {
                    changes.push(Change::Woke);
                }
            }
        }
        if self.since.is_some_and(|since| now - since >= GIVE_UP) {
            self.since = None;
            changes.push(Change::GaveUp);
        }
        changes
    }
}

/// Waits for logind's `PrepareForSleep`, holding a delay lock while awake. Returns why it stopped.
async fn watch(tx: &mpsc::Sender<Notice>) -> anyhow::Error {
    let r: anyhow::Result<()> = async {
        let bus = zbus::Connection::system().await?;
        let logind = zbus::Proxy::new(&bus, "org.freedesktop.login1", "/org/freedesktop/login1", "org.freedesktop.login1.Manager").await?;
        // Listening before the lock is taken, so no sleep falls between the two.
        let mut signals = logind.receive_signal("PrepareForSleep").await?;
        let mut lock = Some(inhibit(&logind).await?);
        while let Some(signal) = signals.next().await {
            let notice = if signal.body().deserialize::<bool>()? {
                Notice::Going(lock.take())
            } else {
                match inhibit(&logind).await {
                    Ok(fd) => lock = Some(fd),
                    Err(e) => {
                        let why = format!("could not take logind's sleep lock again: {e:#}");
                        if tx.send(Notice::Trouble(why)).is_err() {
                            return Ok(());
                        }
                    }
                }
                Notice::Woke
            };
            if tx.send(notice).is_err() {
                return Ok(());
            }
        }
        anyhow::bail!("logind's signals ended")
    }
    .await;
    match r {
        Ok(()) => anyhow::anyhow!("the loop is gone"),
        Err(e) => e.context("watching logind for sleep"),
    }
}

async fn inhibit(logind: &zbus::Proxy<'_>) -> anyhow::Result<OwnedFd> {
    let fd: zbus::zvariant::OwnedFd = logind.call("Inhibit", &("sleep", "Penna", WHY, "delay")).await?;
    Ok(fd.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logind() -> (mpsc::Sender<Notice>, Sleep) {
        let (tx, rx) = mpsc::channel();
        (tx, Sleep { source: Source::Logind(rx), since: None })
    }

    #[test]
    fn the_lock_goes_when_the_loop_takes_the_notice_and_the_wake_comes_once() {
        let (tx, mut sleep) = logind();
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        tx.send(Notice::Going(Some(OwnedFd::from(ours)))).unwrap();
        // logind sees the lock go as its end closes: here, a read that ends.
        theirs.set_nonblocking(true).unwrap();
        let open = |s: &std::os::unix::net::UnixStream| {
            let mut b = [0u8; 1];
            matches!(std::io::Read::read(&mut &*s, &mut b), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
        };
        assert!(open(&theirs), "held until the loop is between ticks");
        assert_eq!(sleep.poll(10, 10.0), [Change::Going]);
        assert!(!open(&theirs), "let go once the loop has it");
        assert!(sleep.asleep());
        assert_eq!(sleep.poll(11, 11.0), []);
        tx.send(Notice::Woke).unwrap();
        tx.send(Notice::Woke).unwrap();
        assert_eq!(sleep.poll(12, 12.0), [Change::Woke]);
        assert!(!sleep.asleep());
    }

    #[test]
    fn without_word_from_logind_the_loop_drives_the_fans_again() {
        let (tx, mut sleep) = logind();
        tx.send(Notice::Going(None)).unwrap();
        assert_eq!(sleep.poll(0, 100.0), [Change::Going]);
        assert_eq!(sleep.poll(29, 129.0), []);
        assert_eq!(sleep.poll(30, 130.0), [Change::GaveUp]);
        assert!(!sleep.asleep());
        tx.send(Notice::Woke).unwrap();
        assert_eq!(sleep.poll(31, 131.0), [], "a wake after giving up is nothing new");
    }

    #[test]
    fn a_script_sleeps_from_one_tick_to_another() {
        let mut sleep = Sleep::script(3, 5);
        let seen: Vec<_> = (0..7).map(|t| sleep.poll(t, t as f64)).collect();
        assert_eq!(seen, [vec![], vec![], vec![], vec![Change::Going], vec![], vec![Change::Woke], vec![]]);
    }
}
