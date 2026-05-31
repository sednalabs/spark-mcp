use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const UNKNOWN: &str = "unknown";

fn main() {
    for key in [
        "SOURCE_DATE_EPOCH",
        "RUSTC",
        "PROFILE",
        "TARGET",
        "MCP_BUILD_COMPONENT",
        "MCP_BUILD_SERVER_VERSION",
        "MCP_BUILD_GIT_SHA",
        "MCP_BUILD_GIT_REF",
        "MCP_BUILD_GIT_DIRTY",
        "MCP_BUILD_TOOLCHAIN",
        "MCP_BUILD_SOURCE_DATE_EPOCH",
        "MCP_BUILD_IDENTITY_OVERRIDE",
        "MCP_BUILD_PRODUCTION",
        "SPARK_MCP_BUILD_COMPONENT",
        "SPARK_MCP_BUILD_SERVER_VERSION",
        "SPARK_MCP_BUILD_GIT_SHA",
        "SPARK_MCP_BUILD_GIT_REF",
        "SPARK_MCP_BUILD_GIT_DIRTY",
        "SPARK_MCP_BUILD_TOOLCHAIN",
        "SPARK_MCP_BUILD_SOURCE_DATE_EPOCH",
        "SPARK_MCP_BUILD_IDENTITY_OVERRIDE",
        "SPARK_MCP_BUILD_PRODUCTION",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }

    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string()));
    configure_git_rerun_inputs(&manifest_dir);

    let production_mode = parse_truthy(env_any(&[
        "SPARK_MCP_BUILD_PRODUCTION",
        "MCP_BUILD_PRODUCTION",
    ]));
    let explicit_server_version =
        env_any(&["SPARK_MCP_BUILD_SERVER_VERSION", "MCP_BUILD_SERVER_VERSION"]);
    let explicit_git_sha = env_any(&["SPARK_MCP_BUILD_GIT_SHA", "MCP_BUILD_GIT_SHA"]);
    if production_mode {
        if explicit_server_version.is_none() {
            panic!(
                "production build requires explicit SPARK_MCP_BUILD_SERVER_VERSION (or MCP_BUILD_SERVER_VERSION)"
            );
        }
        if explicit_git_sha.is_none() {
            panic!(
                "production build requires explicit SPARK_MCP_BUILD_GIT_SHA (or MCP_BUILD_GIT_SHA)"
            );
        }
    }

    emit_env(
        "SPARK_MCP_BUILD_COMPONENT",
        env_any(&["SPARK_MCP_BUILD_COMPONENT", "MCP_BUILD_COMPONENT"])
            .or_else(|| env::var("CARGO_PKG_NAME").ok()),
    );
    emit_env(
        "SPARK_MCP_BUILD_SERVER_VERSION",
        explicit_server_version.or_else(|| env::var("CARGO_PKG_VERSION").ok()),
    );
    emit_env(
        "SPARK_MCP_BUILD_GIT_SHA",
        explicit_git_sha.or_else(|| git_output(&manifest_dir, &["rev-parse", "--verify", "HEAD"])),
    );
    emit_env(
        "SPARK_MCP_BUILD_GIT_REF",
        env_any(&["SPARK_MCP_BUILD_GIT_REF", "MCP_BUILD_GIT_REF"]).or_else(|| {
            git_output(
                &manifest_dir,
                &["symbolic-ref", "--quiet", "--short", "HEAD"],
            )
        }),
    );
    emit_env(
        "SPARK_MCP_BUILD_GIT_DIRTY",
        env_any(&["SPARK_MCP_BUILD_GIT_DIRTY", "MCP_BUILD_GIT_DIRTY"]).or_else(|| {
            git_output(
                &manifest_dir,
                &["status", "--porcelain", "--untracked-files=no"],
            )
            .map(|value| (!value.trim().is_empty()).to_string())
        }),
    );
    emit_env(
        "SPARK_MCP_BUILD_RUSTC_VERSION",
        env_any(&["SPARK_MCP_BUILD_TOOLCHAIN", "MCP_BUILD_TOOLCHAIN"]).or_else(rustc_version),
    );
    emit_env(
        "SPARK_MCP_BUILD_PROFILE",
        env_any(&["SPARK_MCP_BUILD_PROFILE", "MCP_BUILD_PROFILE"])
            .or_else(|| env::var("PROFILE").ok()),
    );
    emit_env(
        "SPARK_MCP_BUILD_TARGET",
        env_any(&["SPARK_MCP_BUILD_TARGET", "MCP_BUILD_TARGET"])
            .or_else(|| env::var("TARGET").ok()),
    );
    emit_env(
        "SPARK_MCP_BUILD_SOURCE_DATE_EPOCH",
        env_any(&[
            "SPARK_MCP_BUILD_SOURCE_DATE_EPOCH",
            "MCP_BUILD_SOURCE_DATE_EPOCH",
            "SOURCE_DATE_EPOCH",
        ]),
    );
    emit_env(
        "SPARK_MCP_BUILD_IDENTITY_OVERRIDE",
        env_any(&[
            "SPARK_MCP_BUILD_IDENTITY_OVERRIDE",
            "MCP_BUILD_IDENTITY_OVERRIDE",
        ]),
    );
}

fn emit_env(key: &str, value: Option<String>) {
    let value = value.unwrap_or_else(|| UNKNOWN.to_string());
    let value = value.trim().replace('\n', " ");
    println!("cargo:rustc-env={key}={value}");
}

fn env_any(keys: &[&str]) -> Option<String> {
    for key in keys {
        let Ok(raw) = env::var(key) else {
            continue;
        };
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn parse_truthy(value: Option<String>) -> bool {
    matches!(
        value
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn rustc_version() -> Option<String> {
    let rustc = env::var("RUSTC").ok()?;
    let output = Command::new(rustc).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    Some(text.trim().to_string())
}

fn git_output(repo_root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

fn configure_git_rerun_inputs(repo_root: &Path) {
    let Some(git_dir) = resolve_git_dir(repo_root) else {
        return;
    };
    println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    println!(
        "cargo:rerun-if-changed={}",
        git_dir.join("packed-refs").display()
    );
    if let Some(reference) = read_head_reference(&git_dir) {
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join(reference).display()
        );
    }
}

fn resolve_git_dir(repo_root: &Path) -> Option<PathBuf> {
    let dot_git = repo_root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let text = fs::read_to_string(dot_git).ok()?;
    let prefix = "gitdir:";
    let gitdir = text.trim().strip_prefix(prefix)?.trim();
    let path = PathBuf::from(gitdir);
    if path.is_absolute() {
        Some(path)
    } else {
        Some(repo_root.join(path))
    }
}

fn read_head_reference(git_dir: &Path) -> Option<String> {
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let prefix = "ref:";
    let reference = head.trim().strip_prefix(prefix)?.trim();
    if reference.is_empty() {
        None
    } else {
        Some(reference.to_string())
    }
}
