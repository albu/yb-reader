//! Bake the git sha (+dirty marker) into the binary so a deployed build
//! is identifiable on the home screen — "did the deploy actually land?"
//! stoppeds being a guess. No rerun-if-changed directives on purpose:
//! the sha must be re-read on every build, since a deploy after any
//! commit (or a dirty tree) is exactly when the question gets asked.

use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "nogit".to_string());
    let dirty = Command::new("git")
        .args(["diff", "--quiet"])
        .status()
        .map(|s| !s.success())
        .unwrap_or(true);
    let v = if dirty {
        format!("{sha}*")
    } else {
        sha
    };
    println!("cargo:rustc-env=YB_BUILD={v}");
}
