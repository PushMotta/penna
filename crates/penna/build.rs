//! `--version` says which build this is: the crate version, plus the short git hash when built from a checkout.
use std::process::Command;

fn main() {
    let version = env!("CARGO_PKG_VERSION");
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
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
