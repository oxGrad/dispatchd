fn main() {
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    let is_tagged = std::process::Command::new("git")
        .args(["describe", "--exact-match", "--tags", "HEAD"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    // The string `dispatchd --version` / `-V` prints. A plain release built
    // from a version tag stays just the crate version; anything else (a local
    // build, a `curl | sh` install off `main`, CI) gets the short SHA appended
    // so it's obvious exactly which commit is running.
    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let version = if is_tagged || sha == "unknown" {
        pkg_version
    } else {
        format!("{pkg_version} ({sha})")
    };

    println!("cargo:rustc-env=DISPATCHD_GIT_SHA={sha}");
    println!("cargo:rustc-env=DISPATCHD_IS_TAGGED={is_tagged}");
    println!("cargo:rustc-env=DISPATCHD_VERSION={version}");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/tags");
}
