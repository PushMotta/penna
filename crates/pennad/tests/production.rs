//! Production mode as an ordinary user can see it (M4): the refusals that keep real hardware safe, `penna restore` asking for
//! root before touching real fans, and a hung loop that stops pinging the watchdog so systemd can kill it (DESIGN.md §6.2).
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn pennad() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pennad"))
}

fn penna() -> PathBuf {
    let bin = pennad().with_file_name("penna");
    if !bin.exists() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let ok = Command::new(cargo)
            .args(["build", "-q", "-p", "penna"])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .status()
            .expect("cargo runs");
        assert!(ok.success(), "building penna");
    }
    bin
}

fn fresh(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("production-{name}"));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

fn desk_config() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/fixtures/desk-3090.config.toml")
}

#[test]
fn an_ordinary_user_is_refused_before_anything_is_touched() {
    if root() {
        return;
    }
    let dir = fresh("not-root");
    let out = Command::new(pennad())
        .arg("--config")
        .arg(desk_config())
        .arg("--runtime-dir")
        .arg(dir.join("run"))
        .arg("--socket")
        .arg(dir.join("penna.sock"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(err.trim(), "pennad controls fans and has to run as root. For a simulated desk: pennad --mock render-burst");
    assert!(!dir.join("run").exists() && !dir.join("penna.sock").exists(), "nothing was created");
}

#[test]
fn a_config_that_assigns_a_motherboard_fan_only_needs_root() {
    if root() {
        return;
    }
    let dir = fresh("motherboard");
    let text = fs::read_to_string(desk_config()).unwrap()
        + "\n[[control]]\nkey = \"hwmon/it87/it87.2608/pwm1\"\nlabel = \"CPU cooler\"\nmin = 25\n";
    let text = text.replace("[profiles.balanced]\n", "[profiles.balanced]\n\"hwmon/it87/it87.2608/pwm1\" = \"gpu\"\n");
    let config = dir.join("config.toml");
    fs::write(&config, text).unwrap();
    let out = Command::new(pennad()).arg("--config").arg(&config).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        err.trim(),
        "pennad controls fans and has to run as root. For a simulated desk: pennad --mock render-burst",
        "M5 drives motherboard fans"
    );
}

#[test]
fn restoring_real_fans_asks_for_root_and_keeps_held_json() {
    if root() {
        return;
    }
    let dir = fresh("restore");
    let held = dir.join("held.json");
    fs::write(
        &held,
        r#"{"version":1,"pid":1,"controls":[{"kind":"nvml","key":"nvml/GPU-168e2769-d673-f36a-ea7b-64df57f8d090/fan0","uuid":"GPU-168e2769-d673-f36a-ea7b-64df57f8d090","fan":0,"original_policy":"driver"}]}"#,
    )
    .unwrap();
    let out =
        Command::new(penna()).args(["restore", "--from"]).arg(&held).arg("--firmware").arg(dir.join("firmware.json")).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("needs root: sudo penna restore --from"), "{err}");
    assert!(held.exists(), "nothing went back, so held.json stays");
}

/// What a client of the daemon's socket sees: the `seq` of the newest tick.
fn seq(socket: &Path) -> u64 {
    let stream = UnixStream::connect(socket).expect("pennad answers");
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    (&stream).write_all(b"{\"id\":1,\"op\":\"state\"}\n").unwrap();
    line.clear();
    reader.read_line(&mut line).unwrap();
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    v["result"]["seq"].as_u64().expect("a seq")
}

#[test]
fn a_hung_loop_stops_ticking_and_stops_pinging_the_watchdog() {
    let dir = fresh("hang");
    let notify = dir.join("notify.sock");
    let listener = UnixDatagram::bind(&notify).unwrap();
    listener.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
    let pings: Arc<Mutex<Vec<(Instant, String)>>> = Arc::default();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let (pings, done) = (pings.clone(), done.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 512];
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                if let Ok(n) = listener.recv(&mut buf) {
                    pings.lock().unwrap().push((Instant::now(), String::from_utf8_lossy(&buf[..n]).into_owned()));
                }
            }
        })
    };

    let socket = dir.join("penna.sock");
    let log_path = dir.join("pennad.log");
    let mut child = Command::new(pennad())
        .args(["--mock", "render-burst", "--speed", "20", "--tick", "0.25", "--forever", "--quiet", "--test-hang-after", "3", "--tree"])
        .arg(dir.join("tree"))
        .arg("--runtime-dir")
        .arg(dir.join("run"))
        .arg("--socket")
        .arg(&socket)
        .env("NOTIFY_SOCKET", &notify)
        .env("WATCHDOG_USEC", "400000")
        .env_remove("WATCHDOG_PID")
        .stdout(Stdio::null())
        .stderr(fs::File::create(&log_path).unwrap())
        .spawn()
        .unwrap();

    let end = Instant::now() + Duration::from_secs(20);
    let hung_at = loop {
        if fs::read_to_string(&log_path).unwrap_or_default().contains("test hang after 3 ticks") {
            break Instant::now();
        }
        assert!(Instant::now() < end, "never hung: {}", fs::read_to_string(&log_path).unwrap_or_default());
        std::thread::sleep(Duration::from_millis(10));
    };
    let first = seq(&socket);
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(seq(&socket), first, "no tick after the hang");
    assert_eq!(first, 3);

    let watchdog: Vec<Instant> = pings.lock().unwrap().iter().filter(|(_, m)| m.contains("WATCHDOG=1")).map(|(t, _)| *t).collect();
    assert!(watchdog.len() >= 3, "the loop pinged while it ticked: {}", watchdog.len());
    let late: Vec<_> = watchdog.iter().filter(|t| **t > hung_at + Duration::from_millis(100)).collect();
    assert!(late.is_empty(), "{} watchdog pings after the hang", late.len());
    assert!(pings.lock().unwrap().iter().any(|(_, m)| m.contains("READY=1")));

    // SIGTERM still stops it, and every fan goes back.
    // SAFETY: kill with a pid we spawned.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let status = child.wait().unwrap();
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    reader.join().unwrap();
    assert!(status.success(), "{}", fs::read_to_string(&log_path).unwrap_or_default());
    assert!(!dir.join("run/held.json").exists(), "every fan was handed back");
}
