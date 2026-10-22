//! The socket (DESIGN.md §7.3, §7.4): pennad runs a mock desk as an ordinary user on its own socket, and a client talks to it
//! the way the shell plugin and the CLI do.
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CPU: &str = "hwmon/it87/it87.2608/pwm1";
const SPARE: &str = "hwmon/it87/it87.2608/pwm4";

fn fresh(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("ipc-{name}"));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

struct Daemon {
    child: Child,
    dir: PathBuf,
}

impl Daemon {
    fn start(dir: &Path, extra: &[&str]) -> Daemon {
        Daemon::start_in(dir, "tree", extra)
    }

    fn start_in(dir: &Path, tree: &str, extra: &[&str]) -> Daemon {
        Daemon::start_scenario(dir, "render-burst", tree, extra)
    }

    fn start_scenario(dir: &Path, scenario: &str, tree: &str, extra: &[&str]) -> Daemon {
        let log = fs::File::create(dir.join("pennad.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_pennad"))
            .args(["--mock", scenario, "--forever", "--quiet", "--tree"])
            .arg(dir.join(tree))
            .arg("--runtime-dir")
            .arg(dir.join("run"))
            .arg("--socket")
            .arg(dir.join("penna.sock"))
            .args(extra)
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("pennad starts");
        Daemon { child, dir: dir.to_path_buf() }
    }

    fn log(&self) -> String {
        fs::read_to_string(self.dir.join("pennad.log")).unwrap_or_default()
    }

    fn connect(&self) -> Client {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(s) = UnixStream::connect(self.dir.join("penna.sock")) {
                return Client::new(s);
            }
            assert!(Instant::now() < end, "pennad never listened; it said:\n{}", self.log());
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn stop(mut self) {
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
        let status = self.child.wait().unwrap();
        assert!(status.success(), "pennad exited badly: {status:?}\n{}", self.log());
        assert!(!self.dir.join("penna.sock").exists(), "a clean stop removes the socket");
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next: u64,
}

impl Client {
    fn new(s: UnixStream) -> Client {
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        Client { writer: s.try_clone().unwrap(), reader: BufReader::new(s), next: 1 }
    }

    fn line(&mut self) -> Value {
        let mut buf = String::new();
        let n = self.reader.read_line(&mut buf).expect("a line in time");
        assert!(n > 0, "pennad closed the connection");
        serde_json::from_str(&buf).unwrap_or_else(|e| panic!("not JSON ({e}): {buf}"))
    }

    fn send(&mut self, v: Value) {
        writeln!(self.writer, "{v}").unwrap();
    }

    /// A request and its reply, skipping ticks and events in between.
    fn call(&mut self, mut req: Value) -> Value {
        let id = self.next;
        self.next += 1;
        req["id"] = json!(id);
        self.send(req);
        loop {
            let v = self.line();
            if v.get("ev").is_none() && v["id"] == json!(id) {
                return v;
            }
        }
    }

    /// The next tick that passes `ok`, within `ticks` ticks.
    fn tick_where(&mut self, ticks: usize, what: &str, ok: impl Fn(&Value) -> bool) -> Value {
        let mut seen = 0;
        loop {
            let v = self.line();
            if v["ev"] == "tick" {
                if ok(&v) {
                    return v;
                }
                seen += 1;
                assert!(seen < ticks, "no tick with {what} in {ticks} ticks; the last was {v}");
            }
        }
    }
}

fn control<'a>(tick: &'a Value, key: &str) -> &'a Value {
    tick["controls"].as_array().unwrap().iter().find(|c| c["key"] == key).unwrap_or(&Value::Null)
}

#[test]
fn a_client_reads_subscribes_and_changes_what_the_daemon_does() {
    let dir = fresh("client");
    let d = Daemon::start(&dir, &["--speed", "20"]);
    let mut c = d.connect();

    let hello = c.line();
    assert_eq!((hello["ev"].as_str(), hello["v"].as_u64(), hello["mock"].as_bool()), (Some("hello"), Some(2), Some(true)));
    assert_eq!(hello["can_write"], true, "the mock desk's own user may write");

    // The socket comes up before the first tick; wait for a few, so there is history to send.
    let mut state = c.call(json!({"op": "state"}));
    while state["result"]["seq"].as_u64() < Some(3) {
        std::thread::sleep(Duration::from_millis(20));
        state = c.call(json!({"op": "state"}));
    }
    assert_eq!(state["ok"], true);
    let s = &state["result"];
    assert_eq!(s["profile"], "balanced");
    assert_eq!(
        s["profiles"].as_array().unwrap().iter().map(|p| p["label"].as_str().unwrap()).collect::<Vec<_>>(),
        ["Defaults", "Quiet", "Balanced"]
    );
    assert_eq!(s["controls"].as_array().unwrap().len(), 5);
    assert_eq!(s["firmware"][0]["key"], SPARE);
    let tctl = s["sensors"].as_array().unwrap().iter().find(|x| x["key"] == "hwmon/k10temp/0000:00:18.3/temp1").unwrap();
    assert_eq!(tctl["scale"], json!([20.0, 95.0]), "TjMax 90 °C + 5");
    assert!(s.get("ev").is_none(), "state's result is the tick body, without ev");

    let sub = c.call(json!({"op": "subscribe"}));
    assert_eq!(sub["ok"], true);
    let history = c.line();
    assert_eq!(history["ev"], "history");
    assert!(history["sensors"]["hwmon/k10temp/0000:00:18.3/temp1"].as_array().is_some_and(|a| !a.is_empty()));
    assert!(history["controls"][CPU]["duty"].as_array().is_some());
    let first = c.line();
    assert_eq!(first["ev"], "tick");
    let later = c.tick_where(50, "a later seq", |t| t["seq"].as_u64() > first["seq"].as_u64());
    assert!(later["t"].as_f64().unwrap() > 1.7e9);

    // profile.set: answered, applied, and kept in the tree's copy of the configuration.
    let r = c.call(json!({"op": "profile.set", "name": "Quiet"}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["result"]["text"], "Profile: Quiet");
    c.tick_where(5, "profile quiet", |t| t["profile"] == "quiet");
    let saved = fs::read_to_string(dir.join("tree/etc/penna/config.toml")).unwrap();
    assert!(saved.contains("profile = \"quiet\""), "{saved}");
    assert_eq!(c.call(json!({"op": "profile.set", "name": "balanced"}))["ok"], true);

    // Defaults hands every fan back and keeps them there; it can't be edited (§4.7).
    let r = c.call(json!({"op": "profile.set", "name": "defaults"}));
    assert_eq!(r["result"]["text"], "Profile: Defaults. Every fan goes back to its BIOS or driver", "{r}");
    c.tick_where(10, "every fan with its firmware", |t| {
        t["profile"] == "defaults"
            && t["controls"].as_array().unwrap().iter().all(|c| matches!(c["state"].as_str(), Some("firmware" | "absent")))
    });
    assert!(fs::read_to_string(dir.join("tree/etc/penna/config.toml")).unwrap().contains("profile = \"defaults\""));
    let r = c.call(json!({"op": "adopt", "control": SPARE, "curve": "Case"}));
    assert!(r["error"].as_str().unwrap().starts_with("Defaults leaves every fan with its BIOS or driver, and can't be changed."), "{r}");
    assert_eq!(c.call(json!({"op": "profile.set", "name": "balanced"}))["ok"], true);
    c.tick_where(10, "the curves again", |t| control(t, CPU)["state"] == "curve");

    // A timed override shows its deadline, then expires on its own.
    let r = c.call(json!({"op": "override", "control": "CPU cooler", "duty": 77, "seconds": 0.6}));
    assert_eq!(r["ok"], true, "{r}");
    let held = c.tick_where(40, "the override", |t| control(t, CPU)["state"] == "override");
    assert_eq!(control(&held, CPU)["override"]["duty"], 77.0);
    assert!(control(&held, CPU)["override"]["until"].as_f64().is_some());
    c.tick_where(200, "the override expiring", |t| control(t, CPU)["override"].is_null() && control(t, CPU)["state"] != "override");

    let r = c.call(json!({"op": "full_speed", "on": true}));
    assert_eq!(r["ok"], true);
    c.tick_where(5, "full speed", |t| t["full_speed"] == true && control(t, CPU)["duty"] == 100.0);
    assert_eq!(c.call(json!({"op": "full_speed", "on": false}))["ok"], true);

    // adopt and release change the active profile and are kept; label renames.
    let r = c.call(json!({"op": "adopt", "control": SPARE, "curve": "Case"}));
    assert_eq!(r["ok"], true, "{r}");
    let t = c.tick_where(10, "pwm4 adopted", |t| control(t, SPARE)["curve"] == "case");
    assert!(t["firmware"].as_array().unwrap().is_empty());
    assert!(fs::read_to_string(dir.join("tree/etc/penna/config.toml")).unwrap().contains(&format!("\"{SPARE}\" = \"case\"")));
    assert_eq!(c.call(json!({"op": "release", "control": SPARE}))["ok"], true);
    c.tick_where(10, "pwm4 released", |t| control(t, SPARE)["state"] == "firmware");
    let r = c.call(json!({"op": "label", "key": CPU, "label": "CPU fan"}));
    assert_eq!(r["result"]["text"], "CPU cooler is now called CPU fan");
    c.tick_where(5, "the new label", |t| control(t, CPU)["label"] == "CPU fan");

    // What a person gets wrong comes back as a sentence, with the id it was sent with.
    let r = c.call(json!({"op": "turbo"}));
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().starts_with("Penna doesn't know the operation \"turbo\""), "{r}");
    let r = c.call(json!({"op": "profile.set", "name": "silent"}));
    assert_eq!(r["error"], "There is no profile \"silent\". Profiles: defaults, balanced, quiet.");
    let r = c.call(json!({"op": "override", "control": "GPU", "duty": 50}));
    assert!(r["error"].as_str().unwrap().contains("could be GPU fan 1 or GPU fan 2"), "{r}");
    let r = c.call(json!({"op": "override", "duty": 140}));
    assert_eq!(r["error"], "A duty is between 0 and 100 %, not 140.");
    c.send(json!("not an object"));
    c.writer.write_all(b"{nope\n").unwrap();
    let mut errors = Vec::new();
    while errors.len() < 2 {
        let v = c.line();
        if v.get("ev").is_none() {
            errors.push(v);
        }
    }
    assert!(errors[0]["error"].as_str().unwrap().starts_with("A request is a JSON object"), "{errors:?}");
    assert!(errors[1]["id"].is_null() && errors[1]["error"].as_str().unwrap().starts_with("That line isn't JSON"), "{errors:?}");

    // Every change was also an event.
    let r = c.call(json!({"op": "full_speed", "on": true}));
    assert_eq!(r["ok"], true);
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let v = c.line();
        if v["ev"] == "event" && v["text"].as_str().unwrap().starts_with("Full speed: every fan at 100 %") {
            assert_eq!(v["level"], "info");
            break;
        }
        assert!(Instant::now() < end, "no event for full speed");
    }
    d.stop();
}

#[test]
fn a_subscriber_that_stops_reading_is_dropped_and_no_one_else_waits() {
    let dir = fresh("slow");
    let d = Daemon::start(&dir, &["--speed", "400", "--tick", "0.25"]);
    // Subscribes, then never reads another byte.
    let mut slow = d.connect();
    slow.line();
    slow.send(json!({"id": 1, "op": "subscribe"}));

    let mut fast = d.connect();
    fast.line();
    fast.call(json!({"op": "subscribe"}));
    let start = Instant::now();
    let mut seqs = Vec::new();
    while start.elapsed() < Duration::from_secs(4) {
        let v = fast.line();
        if v["ev"] == "tick" {
            seqs.push((start.elapsed(), v["seq"].as_u64().unwrap()));
        }
    }
    // The loop kept ticking the whole time, including while the slow client's buffer was full.
    for window in [Duration::from_millis(500), Duration::from_millis(2500), Duration::from_millis(3900)] {
        let before = seqs.iter().filter(|(t, _)| *t <= window).map(|(_, s)| *s).max().unwrap_or(0);
        let after = seqs.iter().filter(|(t, _)| *t <= window + Duration::from_millis(100)).map(|(_, s)| *s).max().unwrap_or(0);
        assert!(after > before, "ticks stalled around {window:?}: {before} → {after}");
    }
    assert!(d.log().contains("dropped a client (uid"), "pennad said:\n{}", d.log());

    // What the slow client left unread drains, then the connection is closed.
    slow.reader.get_ref().set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut rest = Vec::new();
    let n = slow.reader.read_to_end(&mut rest).expect("the daemon closed it");
    assert!(n > 0);
    d.stop();
}

#[test]
fn a_live_socket_is_never_taken_over_and_a_stale_one_is() {
    let dir = fresh("takeover");
    let first = Daemon::start(&dir, &[]);
    first.connect();
    let out = Command::new(env!("CARGO_BIN_EXE_pennad"))
        .args(["--mock", "render-burst", "--seconds", "1", "--quiet", "--tree"])
        .arg(dir.join("tree2"))
        .arg("--runtime-dir")
        .arg(dir.join("run2"))
        .arg("--socket")
        .arg(dir.join("penna.sock"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("another pennad is already listening on"), "{said}");

    // Killed outright, the first daemon leaves its socket behind; the next one checks nobody answers, then takes it.
    let mut first = first;
    first.child.kill().unwrap();
    first.child.wait().unwrap();
    assert!(dir.join("penna.sock").exists());
    let second = Daemon::start_in(&dir, "tree3", &[]);
    let mut c = second.connect();
    assert_eq!(c.line()["ev"], "hello");
    second.stop();
}

fn curve<'a>(v: &'a Value, id: &str) -> &'a Value {
    v["curves"].as_array().unwrap().iter().find(|c| c["id"] == id).unwrap_or(&Value::Null)
}

const SAVED_POINTS: [[f64; 2]; 4] = [[45.0, 30.0], [65.0, 40.0], [78.0, 65.0], [88.0, 100.0]];
const GPU1: &str = "nvml/GPU-5a1d3090-0000-4000-8000-00000000mock/fan0";

/// The studio (DESIGN.md §7.3, M3): a draft previews at once, is refused with its values when it would make a curve fall, is
/// one connection's alone, dies with that connection, and is kept only by `draft.save`.
#[test]
fn drafts_preview_save_discard_and_die_with_their_connection() {
    let dir = fresh("drafts");
    let d = Daemon::start(&dir, &["--speed", "20"]);
    let config = || fs::read_to_string(dir.join("tree/etc/penna/config.toml")).unwrap();
    let mut a = d.connect();
    a.line();
    while a.call(json!({"op": "state"}))["result"]["seq"].as_u64() < Some(3) {
        std::thread::sleep(Duration::from_millis(20));
    }

    // The studio body: curves with the values in force, every control with its limits and calibration, nobody's draft.
    let s = a.call(json!({"op": "studio"}));
    assert_eq!(s["ok"], true, "{s}");
    let s = &s["result"];
    assert_eq!(s["profile"], "balanced");
    let cq = curve(s, "cpu-quiet").clone();
    assert_eq!(cq["points"], json!(SAVED_POINTS));
    assert_eq!((cq["response"].as_f64(), cq["hysteresis"].as_f64()), (Some(5.0), Some(3.0)));
    assert_eq!(cq["source_label"], "AMD CPU · Tctl");
    assert_eq!(cq["used_by"], json!([CPU]));
    assert_eq!(curve(s, "case")["mix"], json!({"op": "max", "of": ["cpu-quiet", "gpu"]}));
    assert_eq!(curve(s, "exhaust")["trigger"], json!({"idle": 35.0, "load": 70.0, "on": 70.0, "off": 60.0}));
    let ctl = |s: &Value, key: &str| s["controls"].as_array().unwrap().iter().find(|c| c["key"] == key).cloned().unwrap_or(Value::Null);
    let cpu = ctl(s, CPU);
    assert_eq!((cpu["stop"].as_f64(), cpu["floor"].as_f64(), cpu["min"].as_f64()), (Some(20.0), Some(25.0), Some(25.0)));
    assert_eq!(cpu["calibration"].as_array().unwrap().last().unwrap(), &json!([100.0, 1800.0]));
    assert!(cpu["nvml"].is_null());
    assert_eq!(ctl(s, GPU1)["nvml"], json!({"min": 30, "max": 100}));
    assert_eq!(ctl(s, SPARE)["label"], "it87 pwm4", "a control no profile names is in the studio too, named for its chip");
    assert_eq!(s["draft"], json!({"mine": false, "dirty": false, "curves": [], "controls": []}));
    assert_eq!(s["profiles"][0], json!({"id": "defaults", "label": "Defaults", "assign": {}, "builtin": true}));
    assert!(s["profiles"].as_array().unwrap().iter().any(|p| p["assign"][CPU] == "cpu-quiet"));

    // A draft previews on the fans at once, and everyone's tick says so.
    assert_eq!(a.call(json!({"op": "subscribe"}))["ok"], true);
    let mut flat_out = cq.clone();
    flat_out["points"] = json!([[20.0, 100.0], [100.0, 100.0]]);
    let r = a.call(json!({"op": "draft.put_curve", "curve": flat_out}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["result"]["draft"], json!({"mine": true, "dirty": true, "curves": ["cpu-quiet"], "controls": []}));
    let t = a.tick_where(40, "the preview", |t| control(t, CPU)["target"] == 100.0);
    assert_eq!(t["draft"], json!({"curves": ["cpu-quiet"], "controls": []}));

    // A curve that falls is refused with its values, and the draft stays as it was.
    let mut dip = cq.clone();
    dip["points"] = json!([[45, 30], [65, 60], [72, 58], [88, 100]]);
    let r = a.call(json!({"op": "draft.put_curve", "curve": dip}));
    assert_eq!(r["error"], "A curve can't fall as the temperature rises: 72 °C → 58 % is below 65 °C → 60 %.");
    assert_eq!(curve(&a.call(json!({"op": "studio"}))["result"], "cpu-quiet")["points"], json!([[20.0, 100.0], [100.0, 100.0]]));
    let r = a.call(json!({"op": "draft.limits", "control": "CPU cooler", "min": 60, "max": 50}));
    assert_eq!(r["error"], "Min (60 %) is above max (50 %).");

    // One studio at a time.
    let mut b = d.connect();
    b.line();
    assert_eq!(b.call(json!({"op": "subscribe"}))["ok"], true);
    let r = b.call(json!({"op": "draft.put_curve", "curve": cq}));
    assert_eq!(r["error"], "Another studio has unsaved changes.");
    assert_eq!(b.call(json!({"op": "draft.save"}))["error"], "Another studio has unsaved changes.");
    let theirs = b.call(json!({"op": "studio"}));
    assert_eq!(theirs["result"]["draft"], json!({"mine": false, "dirty": true, "curves": ["cpu-quiet"], "controls": []}));

    // The studio that made the draft closes without saving: back to what is saved, and everyone is told.
    drop(a);
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let v = b.line();
        if v["ev"] == "event"
            && v["text"].as_str().unwrap().starts_with("The studio closed without saving; back to the saved configuration")
        {
            assert_eq!(v["text"], "The studio closed without saving; back to the saved configuration (CPU quiet)");
            break;
        }
        assert!(Instant::now() < end, "no event for the closed studio");
    }
    b.tick_where(10, "no draft", |t| t["draft"].is_null());
    b.tick_where(200, "the saved curve back on the fan", |t| control(t, CPU)["target"].as_f64() < Some(100.0));
    assert!(!config().contains("[20.0, 100.0]"), "a preview is never saved:\n{}", config());

    // Saved: kept on disk and in force, with nobody's draft left.
    let mut kept = cq.clone();
    kept["points"] = json!([[40.0, 35.0], [65.0, 45.0], [78.0, 70.0], [88.0, 100.0]]);
    assert_eq!(b.call(json!({"op": "draft.put_curve", "curve": kept.clone()}))["ok"], true);
    let r = b.call(json!({"op": "draft.save"}));
    assert_eq!(r["result"]["text"], "Saved: CPU quiet", "{r}");
    assert!(config().contains("[40.0, 35.0]"), "{}", config());
    b.tick_where(10, "no draft after saving", |t| t["draft"].is_null());
    let s = b.call(json!({"op": "studio"}));
    assert_eq!(s["result"]["draft"]["dirty"], false);
    assert_eq!(curve(&s["result"], "cpu-quiet")["points"], kept["points"]);

    // Discarded: back to what was just saved.
    let mut tried = cq.clone();
    tried["points"] = json!([[30.0, 50.0], [90.0, 100.0]]);
    assert_eq!(b.call(json!({"op": "draft.put_curve", "curve": tried}))["ok"], true);
    let r = b.call(json!({"op": "draft.discard"}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["result"]["draft"]["dirty"], false);
    assert_eq!(curve(&r["result"], "cpu-quiet")["points"], kept["points"]);
    assert_eq!(b.call(json!({"op": "draft.save"}))["error"], "There are no unsaved changes.");

    // Assigning in a draft, and a curve that is used can't be deleted.
    let r = b.call(json!({"op": "draft.assign", "control": SPARE, "curve": "case"}));
    assert_eq!(r["result"]["draft"]["controls"], json!([SPARE]), "{r}");
    let r = b.call(json!({"op": "draft.delete_curve", "curve": "gpu"}));
    assert_eq!(
        r["error"],
        "GPU is used by GPU fan 1 in Balanced, GPU fan 2 in Balanced, GPU fan 1 in Quiet, GPU fan 2 in Quiet; give it another curve first."
    );
    let r = b.call(json!({"op": "draft.delete_curve", "curve": "exhaust"}));
    assert_eq!(r["error"], "Exhaust is used by Rear exhaust in Balanced; give it another curve first.");
    assert_eq!(b.call(json!({"op": "draft.assign", "control": "Rear exhaust", "curve": "case"}))["ok"], true);
    let r = b.call(json!({"op": "draft.delete_curve", "curve": "exhaust"}));
    assert_eq!(r["ok"], true, "{r}");
    assert!(curve(&r["result"], "exhaust").is_null());
    assert_eq!(b.call(json!({"op": "draft.discard"}))["ok"], true);
    d.stop();
}

/// A fan a draft took that the saved configuration doesn't name goes back to its firmware when the draft goes, instead of staying
/// where the preview left it.
#[test]
fn a_fan_only_a_draft_adopted_is_handed_back_when_the_draft_goes() {
    let dir = fresh("draft-hand-back");
    let d = Daemon::start(&dir, &["--speed", "20"]);
    let enable = || fs::read_to_string(dir.join("tree/sys/devices/platform/it87.2608/hwmon/hwmon5/pwm4_enable")).unwrap();
    let held = || fs::read_to_string(dir.join("run/held.json")).unwrap_or_default();
    let mut c = d.connect();
    c.line();
    assert_eq!(c.call(json!({"op": "subscribe"}))["ok"], true);
    assert_eq!(enable().trim(), "2", "the BIOS runs it before anyone adopts it");

    for close in [false, true] {
        let r = c.call(json!({"op": "draft.assign", "control": SPARE, "curve": "case"}));
        assert_eq!(r["ok"], true, "{r}");
        c.tick_where(40, "the spare fan taken by the draft", |t| control(t, SPARE)["state"] == "curve");
        assert_eq!(enable().trim(), "1");
        assert!(held().contains("pwm4_enable"), "{}", held());
        if close {
            drop(c);
            c = d.connect();
            c.line();
            assert_eq!(c.call(json!({"op": "subscribe"}))["ok"], true);
        } else {
            assert_eq!(c.call(json!({"op": "draft.discard"}))["ok"], true);
        }
        c.tick_where(40, "the spare fan back with its firmware", |t| {
            control(t, SPARE).is_null() && t["firmware"].as_array().unwrap().iter().any(|f| f["key"] == SPARE)
        });
        assert_eq!(enable().trim(), "2", "handed back when the draft was {}", if close { "closed" } else { "discarded" });
        assert!(!held().contains("pwm4_enable"), "{}", held());
    }
    d.stop();
}

/// `hold_until` says where a held curve lets go (§4.4), and is null otherwise.
#[test]
fn hold_until_names_where_a_held_curve_lets_go() {
    let dir = fresh("hold");
    let d = Daemon::start(&dir, &["--speed", "40"]);
    let mut c = d.connect();
    c.line();
    assert_eq!(c.call(json!({"op": "subscribe"}))["ok"], true);
    let held = c.tick_where(4000, "CPU quiet held", |t| curve(t, "cpu-quiet")["held"] == true);
    let cq = curve(&held, "cpu-quiet");
    let (until, smoothed) = (cq["hold_until"].as_f64().expect("a number while held"), cq["smoothed"].as_f64().unwrap());
    assert!(until < smoothed && until > smoothed - 3.0 - 0.01, "held at {smoothed} °C until {until} °C");
    let free = c.tick_where(4000, "CPU quiet following", |t| curve(t, "cpu-quiet")["held"] == false);
    assert!(curve(&free, "cpu-quiet")["hold_until"].is_null());
    d.stop();
}

/// A driver loaded while pennad runs: its chip under `devices`, then the class link, as the kernel adds them (§3.6).
fn plug(sys: &Path, platform: &str, hwmon: u32, name: &str, enable: &str) -> PathBuf {
    let dir = sys.join(format!("devices/platform/{platform}/hwmon/hwmon{hwmon}"));
    fs::create_dir_all(&dir).unwrap();
    for (file, v) in [("name", name), ("pwm1", "100"), ("pwm1_enable", enable), ("fan1_input", "1200"), ("temp1_input", "41000")] {
        fs::write(dir.join(file), format!("{v}\n")).unwrap();
    }
    std::os::unix::fs::symlink("../..", dir.join("device")).unwrap();
    std::os::unix::fs::symlink(
        format!("../../devices/platform/{platform}/hwmon/hwmon{hwmon}"),
        sys.join(format!("class/hwmon/hwmon{hwmon}")),
    )
    .unwrap();
    dir
}

fn unplug(sys: &Path, platform: &str, hwmon: u32) {
    fs::remove_file(sys.join(format!("class/hwmon/hwmon{hwmon}"))).unwrap();
    fs::remove_dir_all(sys.join(format!("devices/platform/{platform}"))).unwrap();
}

fn event_where(c: &mut Client, what: &str, ok: impl Fn(&str) -> bool) -> Value {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        let v = c.line();
        if v["ev"] == "event" && v["text"].as_str().is_some_and(&ok) {
            return v;
        }
        assert!(Instant::now() < end, "no event for {what}");
    }
}

#[test]
fn a_chip_that_comes_and_goes_while_running_is_found_held_and_given_back() {
    let dir = fresh("hotplug");
    let d = Daemon::start(&dir, &["--speed", "4"]);
    let mut c = d.connect();
    c.line();
    assert_eq!(c.call(json!({"op": "subscribe"}))["ok"], true);
    let sys = dir.join("tree/sys");
    let key = "hwmon/it8792/it87.2656/pwm1";

    // A driver is loaded: the chip appears, its fan with its firmware.
    plug(&sys, "it87.2656", 20, "it8792", "2");
    let e = event_where(&mut c, "the new chip", |t| t.starts_with("Devices changed. appeared: it8792"));
    assert_eq!(e["text"], "Devices changed. appeared: it8792 (Motherboard): 1 fan control, 1 fan, 1 temperature");
    c.tick_where(20, "the new fan, under firmware", |t| t["firmware"].as_array().unwrap().iter().any(|f| f["key"] == key));

    // Adopted, it is taken: manual, with the way back on disk.
    let r = c.call(json!({"op": "adopt", "control": key, "curve": "Case"}));
    assert_eq!(r["ok"], true, "{r}");
    c.tick_where(20, "the new fan on its curve", |t| control(t, key)["state"] == "curve");
    let enable = |hwmon: u32| fs::read_to_string(sys.join(format!("devices/platform/it87.2656/hwmon/hwmon{hwmon}/pwm1_enable"))).unwrap();
    assert_eq!(enable(20).trim(), "1");

    // The driver is unloaded while Penna holds the fan: said loudly, and the fan is absent.
    unplug(&sys, "it87.2656", 20);
    let e = event_where(&mut c, "the held fan going away", |t| t.contains("went away while Penna held it"));
    assert_eq!(e["level"], "alert");
    c.tick_where(20, "the fan absent", |t| control(t, key)["state"] == "absent");

    // Loaded again, as another hwmonN, still manual where Penna left it: Penna carries on, and held.json follows the files.
    plug(&sys, "it87.2656", 21, "it8792", "1");
    c.tick_where(20, "the fan back on its curve", |t| control(t, key)["state"] == "curve");
    let held = fs::read_to_string(dir.join("run/held.json")).unwrap();
    assert!(held.contains("hwmon21/pwm1_enable") && held.contains("\"original_enable\": 2"), "{held}");

    // Stopping puts it back as it was first found, not as Penna left it.
    d.stop();
    assert_eq!(enable(21).trim(), "2");
}

#[test]
fn a_fan_whose_device_was_gone_at_stop_is_put_back_when_it_returns_to_a_later_pennad() {
    let dir = fresh("waiting");
    let sys = dir.join("tree/sys");
    let key = "hwmon/it8792/it87.2656/pwm1";
    let d = Daemon::start(&dir, &["--speed", "4"]);
    let mut c = d.connect();
    c.line();
    assert_eq!(c.call(json!({"op": "subscribe"}))["ok"], true);
    plug(&sys, "it87.2656", 20, "it8792", "2");
    event_where(&mut c, "the new chip", |t| t.starts_with("Devices changed. appeared: it8792"));
    assert_eq!(c.call(json!({"op": "adopt", "control": key, "curve": "Case"}))["ok"], true);
    c.tick_where(20, "the new fan on its curve", |t| control(t, key)["state"] == "curve");
    unplug(&sys, "it87.2656", 20);
    event_where(&mut c, "the held fan going away", |t| t.contains("went away while Penna held it"));
    drop(c);
    // A clean stop, though one fan can't be put back: it waits in held.json.
    d.stop();
    let held = fs::read_to_string(dir.join("run/held.json")).expect("the waiting entry is kept");
    assert!(held.contains(key), "{held}");

    // A later pennad, same runtime directory, a fresh desk whose configuration doesn't name the fan.
    let d = Daemon::start_in(&dir, "tree2", &["--speed", "4"]);
    let mut c = d.connect();
    c.line();
    assert_eq!(c.call(json!({"op": "subscribe"}))["ok"], true);
    assert!(d.log().contains("stay in held.json until their device returns"), "{}", d.log());
    let sys = dir.join("tree2/sys");
    let back = plug(&sys, "it87.2656", 22, "it8792", "1");
    let e = event_where(&mut c, "the fan put back", |t| t.starts_with("Back, and put back as Penna found it"));
    assert_eq!(e["control"], key);
    assert_eq!(fs::read_to_string(back.join("pwm1_enable")).unwrap().trim(), "2");
    let held = fs::read_to_string(dir.join("run/held.json")).unwrap_or_default();
    assert!(!held.contains(key), "nothing is left waiting: {held}");
    d.stop();
}

/// Setup (§5, §11.3) from a fresh install of the desk M5 measured: one client starts it and follows it tick by tick; pairing finds
/// every header, roles come with their reasons, `measured.toml` is written, and every fan ends as it was found.
#[test]
fn setup_measures_the_x570_desk_from_a_fresh_install() {
    let dir = fresh("setup-x570");
    let boards = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/boards");
    let d = Daemon::start_scenario(&dir, "x570-desk", "tree", &["--speed", "50", "--boards", boards.to_str().unwrap()]);
    let it8688 = "hwmon/it8688_4008090b/it87.2624";
    let it8792 = "hwmon/it8792_4008090b/it87.2656";
    let sys = dir.join("tree/sys/devices/platform");
    let enable = |chip: &str, hwmon: u32, n: u32| {
        fs::read_to_string(sys.join(format!("{chip}/hwmon/hwmon{hwmon}/pwm{n}_enable"))).unwrap().trim().to_owned()
    };
    let mut c = d.connect();
    c.line();
    assert_eq!(c.call(json!({"op": "subscribe"}))["ok"], true);

    // Before setup, Defaults lists every tach by its header, from the board database (§11.3 call 3).
    let t = c.tick_where(10, "the tachs", |t| t["tachs"].as_array().is_some_and(|a| !a.is_empty()));
    let tach = |t: &Value, key: &str| t["tachs"].as_array().unwrap().iter().find(|x| x["key"] == key).cloned().unwrap_or(Value::Null);
    assert_eq!(tach(&t, &format!("{it8688}/fan1"))["label"], "CPU_FAN");
    assert_eq!(tach(&t, &format!("{it8792}/fan2"))["label"], "SYS_FAN6_PUMP");
    assert!(tach(&t, &format!("{it8688}/fan1"))["control"].is_null(), "nothing is paired yet");
    assert!(t["setup"].is_null());

    let r = c.call(json!({"op": "setup.start"}));
    assert_eq!(r["ok"], true, "{r}");
    assert!(r["result"]["text"].as_str().unwrap().starts_with("Setup started: 10 fan controls."), "{r}");
    let mut second = d.connect();
    second.line();
    assert_eq!(second.call(json!({"op": "setup.start"}))["error"], "Setup is running in another window.");
    drop(second);

    // Pairing: the bar of a measured fan says measuring, every other fan still on its BIOS.
    let t = c.tick_where(400, "pairing", |t| t["setup"]["step"] == "pairing");
    assert!(t["setup"]["controls"].as_array().unwrap().iter().all(|c| c["write"] == "ok"), "{}", t["setup"]);
    let t = c.tick_where(3000, "review", |t| t["setup"]["step"] == "review");
    let s = &t["setup"];
    assert!(s["stopped"].is_null(), "{s}");
    let ctl = |key: &str| s["controls"].as_array().unwrap().iter().find(|c| c["key"] == key).cloned().unwrap();
    for (n, header) in [(1, "CPU_FAN"), (2, "SYS_FAN1"), (3, "SYS_FAN2"), (4, "PCH_FAN"), (5, "CPU_OPT")] {
        let p = ctl(&format!("{it8688}/pwm{n}"));
        assert_eq!((p["header"].as_str(), p["tachs"].clone()), (Some(header), json!([format!("{it8688}/fan{n}")])), "{p}");
    }
    assert_eq!(ctl(&format!("{it8792}/pwm1"))["header"], "SYS_FAN5_PUMP");
    assert_eq!(ctl(&format!("{it8792}/pwm2"))["paired"], false, "nothing on SYS_FAN6_PUMP");
    assert_eq!(ctl(&format!("{it8792}/pwm3"))["header"], "SYS_FAN4");
    let role = |key: String| ctl(&key)["suggestion"].clone();
    assert_eq!(role(format!("{it8688}/pwm1"))["role"], "cpu");
    let opt = role(format!("{it8688}/pwm5"));
    assert_eq!((opt["role"].as_str(), opt["confirm"].as_bool()), (Some("pump"), Some(true)), "a fast fan on CPU_OPT may be a pump: {opt}");
    assert_eq!(ctl(&format!("{it8688}/pwm5"))["lowest"], 50.0, "so it is held at 50 % until confirmed");
    let pch = role(format!("{it8688}/pwm4"));
    assert_eq!((pch["role"].as_str(), pch["may_stop"].as_bool()), (Some("chipset"), Some(true)), "{pch}");
    let pump = role(format!("{it8792}/pwm1"));
    assert_eq!((pump["role"].as_str(), pump["confirm"].as_bool()), (Some("case"), Some(true)), "{pump}");
    assert_eq!(ctl(&format!("{it8792}/pwm1"))["lowest"], 50.0);

    // Measured, and saved: the tachs in the tick now name the control that turns them.
    let measured = fs::read_to_string(dir.join("tree/var/lib/penna/measured.toml")).expect("measured.toml written");
    let m: toml::Value = toml::from_str(&measured).unwrap();
    let cpu = m["control"].as_array().unwrap().iter().find(|c| c["key"].as_str() == Some(&format!("{it8688}/pwm1"))).unwrap();
    assert_eq!(cpu["tachs"].as_array().unwrap()[0].as_str(), Some(format!("{it8688}/fan1").as_str()));
    assert!(cpu["stop"].as_float().is_some_and(|s| (15.0..=25.0).contains(&s)), "{cpu}");
    assert_eq!(cpu["suggested"].as_str(), Some("cpu"));
    let t = c.tick_where(10, "paired tachs", |t| tach(t, &format!("{it8688}/fan1"))["control"] == format!("{it8688}/pwm1"));
    assert!(tach(&t, &format!("{it8792}/fan2"))["control"].is_null());

    // Every fan back as it was found, and the plan holds nothing the configuration doesn't name.
    let t = c.tick_where(20, "every fan given back", |t| t["controls"].as_array().unwrap().iter().all(|c| c["state"] != "measuring"));
    assert!(t["controls"].as_array().unwrap().iter().all(|x| x["key"].as_str().unwrap().starts_with("nvml/")), "{}", t["controls"]);
    for n in 1..=5 {
        assert_eq!(enable("it87.2624", 4, n), "2", "IT8688E pwm{n} back on its BIOS");
    }
    for n in 1..=3 {
        assert_eq!(enable("it87.2656", 5, n), "2", "IT8792E pwm{n} back on its BIOS");
    }
    assert!(!fs::read_to_string(dir.join("run/held.json")).unwrap_or_default().contains("pwm"), "held.json holds no motherboard fan");

    // Roles confirmed in the review are kept with the measurements. The case fan on a pump header, held at 50 % until now, is
    // measured below it at once, on this connection (§11.3 call 7).
    let r = c.call(json!({"op": "setup.roles", "roles": {format!("{it8792}/pwm1"): "case", "PCH_FAN": "chipset"}}));
    assert_eq!(r["result"]["text"], "2 roles confirmed; measuring SYS_FAN5_PUMP below 50 %, about 30 s", "{r}");
    let measured = fs::read_to_string(dir.join("tree/var/lib/penna/measured.toml")).unwrap();
    assert!(measured.contains("role = \"case\""), "{measured}");
    c.tick_where(200, "calibrating SYS_FAN5_PUMP", |t| t["setup"]["step"] == "calibration");
    let t = c.tick_where(2000, "measured below 50 %", |t| t["setup"]["step"] == "review");
    let p = &t["setup"]["controls"][0];
    assert_eq!((p["header"].as_str(), p["lowest"].is_null()), (Some("SYS_FAN5_PUMP"), true), "{p}");
    assert!(p["stop"].as_f64().is_some_and(|s| s <= 25.0), "it turns well below 50 %: {p}");
    let m: toml::Value = toml::from_str(&fs::read_to_string(dir.join("tree/var/lib/penna/measured.toml")).unwrap()).unwrap();
    let sys5 = m["control"].as_array().unwrap().iter().find(|c| c["key"].as_str() == Some(&format!("{it8792}/pwm1"))).unwrap().clone();
    assert!(sys5.get("lowest").is_none() && sys5["role"].as_str() == Some("case"), "{sys5}");
    assert!(sys5["calibration"].as_array().unwrap()[0][0].as_float() < Some(50.0), "{sys5}");
    let r = c.call(json!({"op": "setup.roles", "roles": {"CPU_FAN": "blower"}}));
    assert_eq!(r["error"], "\"blower\" isn't a role. Roles: cpu, gpu, pump, chipset, laptop, case.");
    d.stop();
}

/// Closing the window that runs setup stops it, and every fan goes back at once (§5).
#[test]
fn setup_stops_with_its_connection() {
    let dir = fresh("setup-closed");
    let d = Daemon::start_scenario(&dir, "x570-desk", "tree", &["--speed", "20"]);
    let enable = || fs::read_to_string(dir.join("tree/sys/devices/platform/it87.2624/hwmon/hwmon4/pwm1_enable")).unwrap();
    let mut a = d.connect();
    a.line();
    assert_eq!(a.call(json!({"op": "subscribe"}))["ok"], true);
    assert_eq!(a.call(json!({"op": "setup.start"}))["ok"], true);
    a.tick_where(800, "pairing", |t| t["setup"]["step"] == "pairing");
    assert_eq!(enable().trim(), "1", "setup holds the fans");
    let mut b = d.connect();
    b.line();
    assert_eq!(b.call(json!({"op": "subscribe"}))["ok"], true);
    drop(a);
    b.tick_where(40, "setup gone", |t| t["setup"].is_null());
    let end = Instant::now() + Duration::from_secs(10);
    while enable().trim() != "2" {
        assert!(Instant::now() < end, "CPU_FAN never went back to its BIOS");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!dir.join("tree/var/lib/penna/measured.toml").exists(), "a stopped run saves nothing");
    d.stop();
}
