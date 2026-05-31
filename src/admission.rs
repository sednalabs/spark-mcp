//! # Startup Admission
//!
//! Enforces startup admission checks against test-gate artifacts before serving.
//!
//! ## Rationale
//! Keep restarts deterministic by requiring explicit gate evidence bound to the
//! running build identity and source fingerprint.
//!
//! ## Security Boundaries
//! * Evaluates only local filesystem artifacts and compile-time provenance data.
//! * Supports explicit break-glass bypass with auditable reason codes.
//!
//! ## References
//! * `agent-ops/docs/design/mcp-fleet-test-gate-contract.md`
//! * `agent-ops/docs/design/mcp-fleet-runtime-admission-policy.md`

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::config::{StartupAdmissionConfig, StartupAdmissionMode, TestGateProfile};
use crate::provenance::RuntimeProvenance;

const CODE_DISABLED: &str = "admission.disabled";
const CODE_OVERRIDE: &str = "admission.override.active";
const CODE_MISSING: &str = "admission.gate.missing";
const CODE_EXPIRED: &str = "admission.gate.expired";
const CODE_STATUS_INVALID: &str = "admission.gate.status_invalid";
const CODE_COMPONENT_MISMATCH: &str = "admission.gate.component_mismatch";
const CODE_LEVEL_MISMATCH: &str = "admission.gate.level_mismatch";
const CODE_BUILD_MISMATCH: &str = "admission.gate.build_mismatch";
const CODE_SOURCE_MISMATCH: &str = "admission.gate.source_mismatch";
const CODE_MANIFEST_MISMATCH: &str = "admission.gate.manifest_mismatch";
const CODE_TIMESTAMP_INVALID: &str = "admission.gate.timestamp_invalid";
const CODE_PROVENANCE_UNAVAILABLE: &str = "admission.runtime.provenance_unavailable";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionOutcome {
    Disabled,
    Bypassed,
    Passed,
    Warning,
    Rejected,
}

#[derive(Debug, Clone)]
pub struct AdmissionEvaluation {
    pub outcome: AdmissionOutcome,
    pub profile: TestGateProfile,
    pub gate_path: PathBuf,
    pub reason_code: Option<String>,
    pub detail: String,
    pub override_active: bool,
}

#[derive(Debug, Deserialize)]
struct GateArtifact {
    schema_version: u32,
    component: String,
    gate_level: String,
    status: String,
    build_identity: String,
    source_fingerprint: String,
    command_manifest_digest: String,
    expires_at: String,
}

pub fn evaluate_startup_admission(
    config: &StartupAdmissionConfig,
    executable_path: &Path,
    runtime: &RuntimeProvenance,
) -> AdmissionEvaluation {
    let gate_path = required_gate_path(config);
    let profile = config.required_profile;
    if matches!(config.mode, StartupAdmissionMode::Off) {
        return AdmissionEvaluation {
            outcome: AdmissionOutcome::Disabled,
            profile,
            gate_path,
            reason_code: Some(CODE_DISABLED.to_string()),
            detail: "startup admission disabled by configuration".to_string(),
            override_active: false,
        };
    }
    if config.bypass {
        let ttl = config.bypass_ttl_s.unwrap_or_default();
        let reason = config
            .bypass_reason
            .as_deref()
            .unwrap_or("unspecified")
            .to_string();
        return AdmissionEvaluation {
            outcome: AdmissionOutcome::Bypassed,
            profile,
            gate_path,
            reason_code: Some(CODE_OVERRIDE.to_string()),
            detail: format!("startup admission bypass active (ttl_s={ttl}, reason={reason})"),
            override_active: true,
        };
    }

    if runtime.build.build_identity.trim().is_empty()
        || runtime.build.source_fingerprint.trim().is_empty()
    {
        return warning_or_reject(
            config.mode,
            profile,
            gate_path,
            CODE_PROVENANCE_UNAVAILABLE,
            "runtime provenance unavailable".to_string(),
        );
    }

    let gate_meta = match std::fs::metadata(&gate_path) {
        Ok(meta) => meta,
        Err(err) => {
            return warning_or_reject(
                config.mode,
                profile,
                gate_path,
                CODE_MISSING,
                format!("required gate artifact missing or unreadable: {err}"),
            );
        }
    };
    let gate_modified = match gate_meta.modified() {
        Ok(ts) => ts,
        Err(err) => {
            return warning_or_reject(
                config.mode,
                profile,
                gate_path,
                CODE_TIMESTAMP_INVALID,
                format!("required gate artifact has no readable modified time: {err}"),
            );
        }
    };
    let exe_modified = match std::fs::metadata(executable_path).and_then(|meta| meta.modified()) {
        Ok(ts) => ts,
        Err(err) => {
            return warning_or_reject(
                config.mode,
                profile,
                gate_path,
                CODE_TIMESTAMP_INVALID,
                format!("failed to read executable modified time: {err}"),
            );
        }
    };

    if is_json_artifact(&gate_path) {
        let contents = match std::fs::read_to_string(&gate_path) {
            Ok(value) => value,
            Err(err) => {
                return warning_or_reject(
                    config.mode,
                    profile,
                    gate_path,
                    CODE_MISSING,
                    format!("failed to read gate artifact JSON: {err}"),
                );
            }
        };
        let artifact = match serde_json::from_str::<GateArtifact>(&contents) {
            Ok(value) => value,
            Err(err) => {
                return warning_or_reject(
                    config.mode,
                    profile,
                    gate_path,
                    CODE_STATUS_INVALID,
                    format!("invalid gate artifact JSON payload: {err}"),
                );
            }
        };
        if artifact.schema_version != 1 {
            return warning_or_reject(
                config.mode,
                profile,
                gate_path,
                CODE_STATUS_INVALID,
                format!(
                    "unsupported gate artifact schema_version {}; expected 1",
                    artifact.schema_version
                ),
            );
        }
        if artifact.component != runtime.build.component {
            return warning_or_reject(
                config.mode,
                profile,
                gate_path,
                CODE_COMPONENT_MISMATCH,
                format!(
                    "gate component mismatch: expected {}, found {}",
                    runtime.build.component, artifact.component
                ),
            );
        }
        if artifact.gate_level != profile.label() {
            return warning_or_reject(
                config.mode,
                profile,
                gate_path,
                CODE_LEVEL_MISMATCH,
                format!(
                    "gate level mismatch: expected {}, found {}",
                    profile.label(),
                    artifact.gate_level
                ),
            );
        }
        if artifact.status != "pass" {
            return warning_or_reject(
                config.mode,
                profile,
                gate_path,
                CODE_STATUS_INVALID,
                format!("gate status is not pass: {}", artifact.status),
            );
        }
        if artifact.build_identity != runtime.build.build_identity {
            return warning_or_reject(
                config.mode,
                profile,
                gate_path,
                CODE_BUILD_MISMATCH,
                format!(
                    "gate build_identity mismatch: expected {}, found {}",
                    runtime.build.build_identity, artifact.build_identity
                ),
            );
        }
        if artifact.source_fingerprint != runtime.build.source_fingerprint {
            return warning_or_reject(
                config.mode,
                profile,
                gate_path,
                CODE_SOURCE_MISMATCH,
                format!(
                    "gate source_fingerprint mismatch: expected {}, found {}",
                    runtime.build.source_fingerprint, artifact.source_fingerprint
                ),
            );
        }
        if !artifact.command_manifest_digest.starts_with("sha256:") {
            return warning_or_reject(
                config.mode,
                profile,
                gate_path,
                CODE_MANIFEST_MISMATCH,
                "gate command_manifest_digest must start with sha256:".to_string(),
            );
        }
        let expires_at = match OffsetDateTime::parse(&artifact.expires_at, &Rfc3339) {
            Ok(value) => value,
            Err(err) => {
                return warning_or_reject(
                    config.mode,
                    profile,
                    gate_path,
                    CODE_TIMESTAMP_INVALID,
                    format!("gate expires_at is not valid RFC3339: {err}"),
                );
            }
        };
        if OffsetDateTime::now_utc() > expires_at {
            return warning_or_reject(
                config.mode,
                profile,
                gate_path,
                CODE_EXPIRED,
                format!("gate artifact expired at {}", artifact.expires_at),
            );
        }
    }

    if is_stale(gate_modified, exe_modified) {
        return warning_or_reject(
            config.mode,
            profile,
            gate_path,
            CODE_EXPIRED,
            format!(
                "required gate artifact is older than executable ({})",
                executable_path.display()
            ),
        );
    }

    AdmissionEvaluation {
        outcome: AdmissionOutcome::Passed,
        profile,
        gate_path,
        reason_code: None,
        detail: "startup admission checks passed".to_string(),
        override_active: false,
    }
}

fn required_gate_path(config: &StartupAdmissionConfig) -> PathBuf {
    match config.required_profile {
        TestGateProfile::Fast => config.fast_gate_artifact_path.clone(),
        TestGateProfile::Standard => config.standard_gate_artifact_path.clone(),
    }
}

fn warning_or_reject(
    mode: StartupAdmissionMode,
    profile: TestGateProfile,
    gate_path: PathBuf,
    reason_code: &str,
    detail: String,
) -> AdmissionEvaluation {
    let outcome = match mode {
        StartupAdmissionMode::Strict => AdmissionOutcome::Rejected,
        StartupAdmissionMode::Warn => AdmissionOutcome::Warning,
        StartupAdmissionMode::Off => AdmissionOutcome::Disabled,
    };
    AdmissionEvaluation {
        outcome,
        profile,
        gate_path,
        reason_code: Some(reason_code.to_string()),
        detail,
        override_active: false,
    }
}

fn is_json_artifact(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("json"))
        .unwrap_or(false)
}

fn is_stale(gate_modified: SystemTime, exe_modified: SystemTime) -> bool {
    gate_modified < exe_modified
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{StartupAdmissionConfig, StartupAdmissionMode, TestGateProfile};
    use crate::provenance::capture_runtime_provenance;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use time::Duration;
    use time::OffsetDateTime;

    fn temp_path(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        let nonce = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("{prefix}-{nonce}"))
    }

    fn base_config(mode: StartupAdmissionMode) -> StartupAdmissionConfig {
        StartupAdmissionConfig {
            mode,
            required_profile: TestGateProfile::Fast,
            fast_gate_artifact_path: temp_path("spark-fast-gate").join("fast.json"),
            standard_gate_artifact_path: temp_path("spark-standard-gate").join("standard.json"),
            bypass: false,
            bypass_reason: None,
            bypass_ttl_s: None,
            production_mode: false,
            allow_production_bypass: false,
        }
    }

    fn write_gate_json(path: &Path, runtime: &RuntimeProvenance, status: &str, expires_at: &str) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let payload = serde_json::json!({
            "schema_version": 1,
            "component": runtime.build.component,
            "gate_level": "fast",
            "status": status,
            "build_identity": runtime.build.build_identity,
            "source_fingerprint": runtime.build.source_fingerprint,
            "command_manifest_digest": "sha256:test",
            "expires_at": expires_at
        });
        fs::write(path, serde_json::to_vec(&payload).expect("serialize")).expect("write gate");
    }

    #[test]
    fn admission_warn_mode_allows_missing_gate() {
        let config = base_config(StartupAdmissionMode::Warn);
        let exe = temp_path("spark-exe");
        fs::write(&exe, "bin").expect("write exe");
        let runtime = capture_runtime_provenance(&exe);
        let result = evaluate_startup_admission(&config, &exe, &runtime);
        assert_eq!(result.outcome, AdmissionOutcome::Warning);
        let _ = fs::remove_file(exe);
    }

    #[test]
    fn admission_strict_rejects_missing_gate() {
        let config = base_config(StartupAdmissionMode::Strict);
        let exe = temp_path("spark-exe");
        fs::write(&exe, "bin").expect("write exe");
        let runtime = capture_runtime_provenance(&exe);
        let result = evaluate_startup_admission(&config, &exe, &runtime);
        assert_eq!(result.outcome, AdmissionOutcome::Rejected);
        let _ = fs::remove_file(exe);
    }

    #[test]
    fn admission_strict_passes_with_valid_gate_json() {
        let config = base_config(StartupAdmissionMode::Strict);
        let exe = temp_path("spark-exe");
        fs::write(&exe, "bin").expect("write exe");
        std::thread::sleep(std::time::Duration::from_millis(25));
        let runtime = capture_runtime_provenance(&exe);
        let expires = (OffsetDateTime::now_utc() + Duration::hours(1))
            .format(&Rfc3339)
            .expect("format expires");
        write_gate_json(&config.fast_gate_artifact_path, &runtime, "pass", &expires);
        let result = evaluate_startup_admission(&config, &exe, &runtime);
        assert_eq!(result.outcome, AdmissionOutcome::Passed);
        let _ = fs::remove_file(exe);
        let _ = fs::remove_file(&config.fast_gate_artifact_path);
    }

    #[test]
    fn admission_strict_rejects_expired_gate_json() {
        let config = base_config(StartupAdmissionMode::Strict);
        let exe = temp_path("spark-exe");
        fs::write(&exe, "bin").expect("write exe");
        let runtime = capture_runtime_provenance(&exe);
        let expires = (OffsetDateTime::now_utc() - Duration::minutes(5))
            .format(&Rfc3339)
            .expect("format expires");
        write_gate_json(&config.fast_gate_artifact_path, &runtime, "pass", &expires);
        let result = evaluate_startup_admission(&config, &exe, &runtime);
        assert_eq!(result.outcome, AdmissionOutcome::Rejected);
        let _ = fs::remove_file(exe);
        let _ = fs::remove_file(&config.fast_gate_artifact_path);
    }
}
