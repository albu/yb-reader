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
    // `status --porcelain --untracked-files=no` covers staged AND
    // unstaged tracked edits — `diff --quiet` missed staged-only edits,
    // so a deploy built from a staged tree reported a clean sha.
    // Untracked files are excluded: scratch in the tree is not part of
    // what got compiled unless it is `mod`-referenced, and marking every
    // deploy dirty over stray files erases the signal.
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(true);
    let v = if dirty { format!("{sha}*") } else { sha };
    println!("cargo:rustc-env=YB_BUILD={v}");
}
