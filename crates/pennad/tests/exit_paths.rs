//! Every way out hands the fans back (DESIGN.md §6.2, §9.2): pennad runs a mock desk as an ordinary user, then is killed,
//! stopped or made to panic, and every `pwmN_enable` and GPU policy must end exactly as the firmware had it.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const UUID: &str = "GPU-5a1d3090-0000-4000-8000-00000000mock";

fn pennad() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pennad"))
}

/// `penna` is another package's binary: it sits beside pennad when the workspace is built, and is built here when it is not.
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
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("exit-{name}"));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn start(dir: &Path, extra: &[&str]) -> Child {
    let log = fs::File::create(dir.join("pennad.log")).unwrap();
    Command::new(pennad())
        .args(["--mock", "render-burst", "--speed", "20", "--quiet", "--tree"])
        .arg(dir.join("tree"))
        .arg("--runtime-dir")
        .arg(dir.join("run"))
        .arg("--socket")
        .arg(dir.join("penna.sock"))
        .args(extra)
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .expect("pennad starts")
}

fn it87(dir: &Path, file: &str) -> String {
    fs::read_to_string(dir.join("tree/sys/devices/platform/it87.2608/hwmon/hwmon5").join(file)).unwrap_or_default().trim().to_owned()
}

fn policy(dir: &Path, fan: u32) -> String {
    fs::read_to_string(dir.join(format!("tree/sys/penna-mock/nvml/{UUID}/fan{fan}_policy"))).unwrap_or_default().trim().to_owned()
}

fn held(dir: &Path) -> PathBuf {
    dir.join("run/held.json")
}

fn wait_until(what: &str, timeout: Duration, mut ok: impl FnMut() -> bool, dir: &Path) {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}; pennad said:\n{}", fs::read_to_string(dir.join("pennad.log")).unwrap_or_default());
}

fn wait_exit(child: &mut Child, dir: &Path) -> ExitStatus {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        if Instant::now() > end {
            let _ = child.kill();
            panic!("pennad did not exit; it said:\n{}", fs::read_to_string(dir.join("pennad.log")).unwrap_or_default());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Every adopted control in manual, and the GPU fans written (the render has warmed the GPU past its 30 % floor).
fn running_hot(dir: &Path) -> bool {
    let held = fs::read_to_string(held(dir)).unwrap_or_default();
    held.matches("\"key\"").count() == 5
        && ["pwm1_enable", "pwm2_enable", "pwm3_enable"].iter().all(|f| it87(dir, f) == "1")
        && policy(dir, 0) == "manual"
        && policy(dir, 1) == "manual"
}

fn assert_firmware(dir: &Path) {
    assert_eq!(it87(dir, "pwm1_enable"), "2");
    assert_eq!(it87(dir, "pwm2_enable"), "2");
    assert_eq!(it87(dir, "pwm3_enable"), "5", "the exact original value, not an assumed 2");
    assert_eq!(it87(dir, "pwm4_enable"), "2", "never adopted, never touched");
    assert_eq!((policy(dir, 0), policy(dir, 1)), ("driver".into(), "driver".into()));
}

#[test]
fn kill_9_then_penna_restore() {
    let dir = fresh("kill9");
    let mut child = start(&dir, &["--forever"]);
    wait_until("the render to warm every fan into manual", Duration::from_secs(20), || running_hot(&dir), &dir);
    child.kill().unwrap(); // SIGKILL: nothing in pennad runs
    child.wait().unwrap();
    assert_eq!(it87(&dir, "pwm1_enable"), "1", "after kill -9 the fans are still Penna's: this is what restore is for");
    assert_eq!(policy(&dir, 0), "manual");
    let out = Command::new(penna()).arg("restore").arg("--from").arg(held(&dir)).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_firmware(&dir);
    assert!(!held(&dir).exists(), "held.json goes once everything is back");
}

#[test]
fn sigterm_hands_back_and_exits_cleanly() {
    let dir = fresh("sigterm");
    let mut child = start(&dir, &["--forever"]);
    wait_until("every fan in manual", Duration::from_secs(20), || running_hot(&dir), &dir);
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let status = wait_exit(&mut child, &dir);
    assert!(status.success(), "{status:?}");
    assert_firmware(&dir);
    assert!(!held(&dir).exists());
}

#[test]
fn a_panic_hands_back_from_the_hook() {
    let dir = fresh("panic");
    let mut child = start(&dir, &["--forever", "--test-panic-after", "45"]);
    let status = wait_exit(&mut child, &dir);
    let log = fs::read_to_string(dir.join("pennad.log")).unwrap();
    assert_eq!(status.code(), Some(101), "{log}");
    assert!(log.contains("test panic after 45 ticks") && log.contains("handed every fan back"), "{log}");
    assert_firmware(&dir);
    assert!(!held(&dir).exists());
}

#[test]
fn it_refuses_real_motherboard_fans_until_m5() {
    // Without --mock or --sysfs-root it is production, which needs root.
    let out = Command::new(pennad()).output().unwrap();
    assert!(!out.status.success());
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        assert!(String::from_utf8_lossy(&out.stderr).contains("has to run as root"), "{}", String::from_utf8_lossy(&out.stderr));
    }
    let cfg = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/fixtures/scenarios/mock-desk.config.toml");
    let out = Command::new(pennad()).args(["--sysfs-root", "/sys", "--config"]).arg(cfg).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("real /sys"), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_held_json_left_by_a_dead_daemon_is_restored_first() {
    let dir = fresh("leftover");
    let mut child = start(&dir, &["--forever"]);
    wait_until("every fan in manual", Duration::from_secs(20), || running_hot(&dir), &dir);
    child.kill().unwrap();
    child.wait().unwrap();
    // A new daemon on a new desk, but the same runtime directory: the old fans go back before anything else.
    let log = fs::File::create(dir.join("pennad2.log")).unwrap();
    let status = Command::new(pennad())
        .args(["--mock", "render-burst", "--speed", "50", "--quiet", "--seconds", "1", "--tree"])
        .arg(dir.join("tree2"))
        .arg("--runtime-dir")
        .arg(dir.join("run"))
        .arg("--socket")
        .arg(dir.join("penna2.sock"))
        .stderr(log)
        .status()
        .unwrap();
    assert!(status.success(), "{}", fs::read_to_string(dir.join("pennad2.log")).unwrap());
    assert!(fs::read_to_string(dir.join("pennad2.log")).unwrap().contains("restored what a previous run left"));
    assert_firmware(&dir);
}
