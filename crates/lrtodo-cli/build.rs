use std::process::Command;

fn git(args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Versión derivada de git: `{YYYYMMDDHHMM}-{rama}-{Nºcommits}` (ej.
/// `20251031-1840-main-0156`). `rerun-if-changed` en HEAD + la rama para
/// que cada commit recompile el dato.
fn main() {
    let gitdir = git(&["rev-parse", "--git-dir"]);
    if !gitdir.is_empty() {
        println!("cargo:rerun-if-changed={gitdir}/HEAD");
        let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"]);
        println!("cargo:rerun-if-changed={gitdir}/refs/heads/{branch}");
    }
    let date = git(&[
        "log",
        "-1",
        "--format=%cd",
        "--date=format-local:%Y%m%d-%H%M",
    ]);
    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let count = git(&["rev-list", "--count", "HEAD"]);
    let version = if date.is_empty() || count.is_empty() {
        "dev".to_string()
    } else {
        format!("{date}-{branch}-{count:0>4}")
    };
    println!("cargo:rustc-env=LRTODO_VERSION={version}");
}
