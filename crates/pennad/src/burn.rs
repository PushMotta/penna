//! Setup's load check on a real machine (DESIGN.md §5.5): every CPU thread busy at the lowest priority, so anything else the
//! machine does comes first, until told to stop. The mock desk loads its simulated CPU instead.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
pub struct Burn {
    stop: Option<Arc<AtomicBool>>,
}

impl Burn {
    /// Starts or stops the load to match `on`.
    pub fn set(&mut self, on: bool) {
        match (on, &self.stop) {
            (true, None) => {
                let stop = Arc::new(AtomicBool::new(false));
                let n = std::thread::available_parallelism().map_or(1, |n| n.get());
                for i in 0..n {
                    let stop = stop.clone();
                    let _ = std::thread::Builder::new().name(format!("penna-load-{i}")).spawn(move || {
                        // Linux keeps a nice value per thread: this one alone goes to 19, and the control loop keeps its own.
                        // SAFETY: setpriority only reads its arguments; gettid can't fail.
                        unsafe { libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, 19) };
                        let mut x: u64 = 1;
                        while !stop.load(Ordering::Relaxed) {
                            for _ in 0..100_000 {
                                x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                            }
                            std::hint::black_box(x);
                        }
                    });
                }
                self.stop = Some(stop);
            }
            (false, Some(stop)) => {
                stop.store(true, Ordering::Relaxed);
                self.stop = None;
            }
            _ => {}
        }
    }
}

impl Drop for Burn {
    fn drop(&mut self) {
        self.set(false);
    }
}
