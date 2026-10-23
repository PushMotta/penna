//! `--version` says which build this is: the crate version, plus the short git hash when built from a checkout.
use std::process::Command;

fn main() {
    let version = env!("CARGO_PKG_VERSION");
    // Only this checkout's own hash: a release tarball unpacked inside another repository (an AUR helper's clone) has none.
    let workspace = std::fs::canonicalize(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).ok();
    let hash = Command::new("git")
        .args(["rev-parse", "--show-toplevel", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let out = String::from_utf8_lossy(&o.stdout).into_owned();
            let mut lines = out.lines();
            let top = std::fs::canonicalize(lines.next()?).ok();
            (top.is_some() && top == workspace).then(|| lines.next().unwrap_or("").trim().to_owned())
        })
        .filter(|h| !h.is_empty());
    match hash {
        Some(h) => println!("cargo:rustc-env=PENNA_VERSION={version} ({h})"),
        None => println!("cargo:rustc-env=PENNA_VERSION={version}"),
    }
    // A new commit changes HEAD's target; a branch switch changes HEAD itself.
    for path in ["../../.git/HEAD", "../../.git/refs/heads"] {
        println!("cargo:rerun-if-changed={path}");
    }
}
