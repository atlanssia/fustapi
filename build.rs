fn main() {
    let version = git_describe().unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let out_dir = std::env::var("OUT_DIR").unwrap();
    std::fs::write(std::path::Path::new(&out_dir).join("version.txt"), version).unwrap();
}

/// Derive a version string from git.
///
/// Format: `<tag>-<commits-since>-g<short>` (e.g. `1.2.56-3-g86dcc98`),
/// suffixed with `-dirty` only when **Rust source** files (`.rs`) have
/// uncommitted changes.
///
/// We intentionally do NOT use `git describe --dirty`: that flag marks the
/// tree dirty for *any* tracked-file change, including `Cargo.lock` being
/// bumped by `cargo build` itself — which would taint every release binary
/// with a spurious `-dirty` suffix. Restricting the check to `.rs` files
/// keeps the marker meaningful (uncommitted source edits) while letting
/// reproducible builds produce clean version strings.
fn git_describe() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["describe", "--tags", "--always"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let mut v = raw.strip_prefix('v').map(|s| s.to_string()).unwrap_or(raw);

    // Append -dirty only when Rust source files have uncommitted changes.
    // Cargo.lock / build artifacts / docs do not count.
    let dirty = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout).lines().any(|line| {
                // porcelain format: "XY path" where XY is 2 status chars.
                // Path may be quoted if it contains spaces.
                let path = line.get(3..).unwrap_or("").trim_matches('"');
                path.ends_with(".rs")
            })
        })
        .unwrap_or(false);

    if dirty {
        v.push_str("-dirty");
    }
    Some(v)
}
