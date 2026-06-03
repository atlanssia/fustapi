fn main() {
    let version = git_describe().unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let out_dir = std::env::var("OUT_DIR").unwrap();
    std::fs::write(std::path::Path::new(&out_dir).join("version.txt"), version).unwrap();
}

fn git_describe() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Some(v.strip_prefix('v').map(|s| s.to_string()).unwrap_or(v))
}
