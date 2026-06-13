use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=PROFILE");
    println!("cargo:rerun-if-env-changed=TARGET");
    register_git_rerun_files();

    println!(
        "cargo:rustc-env=ORODRUIN_GIT_HASH={}",
        git_output(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into())
    );
    println!(
        "cargo:rustc-env=ORODRUIN_GIT_DATE={}",
        git_output(&["log", "-1", "--date=short", "--format=%cd", "HEAD"])
            .unwrap_or_else(|| "unknown".into())
    );
    println!(
        "cargo:rustc-env=ORODRUIN_BUILD_PROFILE={}",
        std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into())
    );
    println!(
        "cargo:rustc-env=ORODRUIN_BUILD_TARGET={}",
        std::env::var("TARGET").unwrap_or_else(|_| "unknown".into())
    );
}

fn register_git_rerun_files() {
    let Some(git_dir) = git_output(&["rev-parse", "--git-dir"]) else {
        return;
    };

    println!("cargo:rerun-if-changed={git_dir}/HEAD");
    println!("cargo:rerun-if-changed={git_dir}/packed-refs");
    println!("cargo:rerun-if-changed={git_dir}/refs");
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8(output.stdout).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}
