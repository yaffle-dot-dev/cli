use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const TOFU_OVERRIDE_ENV_VAR: &str = "YAFFLE_TOFU_PATH";
pub const SUPPORTED_TOFU_VERSION_RANGE: &str = ">=1.8.0,<2.0.0";
const DEFAULT_TOFU_BINARY_NAME: &str = "tofu";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TofuSourceKind {
    Override,
    Bundled,
    Managed,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TofuVersionRequirement {
    pub pinned_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TofuResolutionPolicy {
    pub preferred_sources: Vec<TofuSourceKind>,
    pub system_fallback_allowed: bool,
}

impl Default for TofuResolutionPolicy {
    fn default() -> Self {
        Self {
            preferred_sources: vec![
                TofuSourceKind::Override,
                TofuSourceKind::Bundled,
                TofuSourceKind::Managed,
                TofuSourceKind::System,
            ],
            system_fallback_allowed: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TofuResolution {
    pub source: TofuSourceKind,
    pub path: PathBuf,
    pub version: String,
}

impl TofuResolution {
    pub fn command(&self) -> Command {
        Command::new(&self.path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TofuResolutionRequest {
    pub policy: TofuResolutionPolicy,
    pub override_path: Option<PathBuf>,
    pub bundled_path: Option<PathBuf>,
    pub managed_path: Option<PathBuf>,
    pub system_search_paths: Option<Vec<PathBuf>>,
    pub version_requirement: Option<TofuVersionRequirement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TofuResolutionReport {
    pub selected: Option<TofuResolution>,
    pub attempts: Vec<TofuResolutionAttempt>,
}

impl TofuResolutionReport {
    pub fn into_result(self) -> Result<TofuResolution, TofuResolutionError> {
        match self.selected {
            Some(resolution) => Ok(resolution),
            None => Err(TofuResolutionError {
                attempts: self.attempts,
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TofuResolutionAttempt {
    pub source: TofuSourceKind,
    pub outcome: TofuAttemptOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TofuAttemptOutcome {
    Resolved(TofuResolution),
    Unavailable(TofuUnavailability),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TofuUnavailability {
    pub reason: TofuUnavailabilityReason,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TofuUnavailabilityReason {
    NotConfigured,
    SystemFallbackDisabled,
    NotFound,
    NotExecutable,
    VersionProbeFailed,
    VersionMismatch,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("could not resolve tofu using the configured source policy")]
pub struct TofuResolutionError {
    pub attempts: Vec<TofuResolutionAttempt>,
}

pub fn inspect_tofu_resolution(request: &TofuResolutionRequest) -> TofuResolutionReport {
    let mut attempts = Vec::new();

    for source in &request.policy.preferred_sources {
        if *source == TofuSourceKind::System && !request.policy.system_fallback_allowed {
            attempts.push(TofuResolutionAttempt {
                source: *source,
                outcome: TofuAttemptOutcome::Unavailable(TofuUnavailability {
                    reason: TofuUnavailabilityReason::SystemFallbackDisabled,
                    message: "system tofu fallback is disabled by policy".to_string(),
                    path: None,
                }),
            });
            continue;
        }

        let attempt = inspect_source(*source, request);
        let selected = match &attempt.outcome {
            TofuAttemptOutcome::Resolved(resolution) => Some(resolution.clone()),
            TofuAttemptOutcome::Unavailable(_) => None,
        };
        attempts.push(attempt);

        if let Some(selected) = selected {
            return TofuResolutionReport {
                selected: Some(selected),
                attempts,
            };
        }
    }

    TofuResolutionReport {
        selected: None,
        attempts,
    }
}

pub fn resolve_tofu(
    request: &TofuResolutionRequest,
) -> Result<TofuResolution, TofuResolutionError> {
    inspect_tofu_resolution(request).into_result()
}

fn inspect_source(
    source: TofuSourceKind,
    request: &TofuResolutionRequest,
) -> TofuResolutionAttempt {
    let candidate = match source {
        TofuSourceKind::Override => request
            .override_path
            .clone()
            .or_else(override_path_from_env),
        TofuSourceKind::Bundled => request.bundled_path.clone(),
        TofuSourceKind::Managed => request.managed_path.clone(),
        TofuSourceKind::System => discover_system_tofu(request.system_search_paths.as_deref()),
    };

    let Some(candidate) = candidate else {
        return TofuResolutionAttempt {
            source,
            outcome: TofuAttemptOutcome::Unavailable(TofuUnavailability {
                reason: missing_candidate_reason(source),
                message: source_not_configured_message(source),
                path: None,
            }),
        };
    };

    if !candidate.is_file() {
        return TofuResolutionAttempt {
            source,
            outcome: TofuAttemptOutcome::Unavailable(TofuUnavailability {
                reason: TofuUnavailabilityReason::NotFound,
                message: format!("tofu candidate '{}' does not exist", candidate.display()),
                path: Some(candidate),
            }),
        };
    }

    match probe_tofu_binary(&candidate) {
        Ok(version) => {
            if let Some(requirement) = &request.version_requirement {
                if version != requirement.pinned_version {
                    return TofuResolutionAttempt {
                        source,
                        outcome: TofuAttemptOutcome::Unavailable(TofuUnavailability {
                            reason: TofuUnavailabilityReason::VersionMismatch,
                            message: format!(
                                "tofu at '{}' resolved version '{}' but policy requires '{}'",
                                candidate.display(),
                                version,
                                requirement.pinned_version
                            ),
                            path: Some(candidate),
                        }),
                    };
                }
            } else if !is_supported_tofu_version(&version) {
                return TofuResolutionAttempt {
                    source,
                    outcome: TofuAttemptOutcome::Unavailable(TofuUnavailability {
                        reason: TofuUnavailabilityReason::VersionMismatch,
                        message: format!(
                            "tofu at '{}' resolved version '{}' but Yaffle v0.1 supports '{}'",
                            candidate.display(),
                            version,
                            SUPPORTED_TOFU_VERSION_RANGE,
                        ),
                        path: Some(candidate),
                    }),
                };
            }

            TofuResolutionAttempt {
                source,
                outcome: TofuAttemptOutcome::Resolved(TofuResolution {
                    source,
                    path: candidate,
                    version,
                }),
            }
        }
        Err(reason) => TofuResolutionAttempt {
            source,
            outcome: TofuAttemptOutcome::Unavailable(TofuUnavailability {
                reason: reason.reason,
                message: reason.message,
                path: Some(candidate),
            }),
        },
    }
}

fn override_path_from_env() -> Option<PathBuf> {
    let value = env::var_os(TOFU_OVERRIDE_ENV_VAR)?;
    if value.is_empty() {
        return None;
    }

    Some(PathBuf::from(value))
}

fn discover_system_tofu(system_search_paths: Option<&[PathBuf]>) -> Option<PathBuf> {
    system_search_paths
        .map(|paths| paths.to_vec())
        .unwrap_or_else(|| search_paths_from_env().unwrap_or_default())
        .into_iter()
        .map(|directory| directory.join(tofu_binary_name()))
        .find(|candidate| candidate.is_file())
}

fn search_paths_from_env() -> Option<Vec<PathBuf>> {
    let path = env::var_os("PATH")?;
    Some(env::split_paths(&path).collect())
}

fn tofu_binary_name() -> OsString {
    if env::consts::EXE_EXTENSION.is_empty() {
        OsString::from(DEFAULT_TOFU_BINARY_NAME)
    } else {
        OsString::from(format!(
            "{DEFAULT_TOFU_BINARY_NAME}.{}",
            env::consts::EXE_EXTENSION
        ))
    }
}

fn source_not_configured_message(source: TofuSourceKind) -> String {
    match source {
        TofuSourceKind::Override => format!(
            "no tofu override path was provided and '{}' is not set",
            TOFU_OVERRIDE_ENV_VAR
        ),
        TofuSourceKind::Bundled => "no bundled tofu path is configured".to_string(),
        TofuSourceKind::Managed => "no managed tofu path is configured".to_string(),
        TofuSourceKind::System => "system PATH did not contain a tofu binary".to_string(),
    }
}

fn missing_candidate_reason(source: TofuSourceKind) -> TofuUnavailabilityReason {
    match source {
        TofuSourceKind::Override | TofuSourceKind::Bundled | TofuSourceKind::Managed => {
            TofuUnavailabilityReason::NotConfigured
        }
        TofuSourceKind::System => TofuUnavailabilityReason::NotFound,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProbeFailure {
    reason: TofuUnavailabilityReason,
    message: String,
}

fn probe_tofu_binary(path: &Path) -> Result<String, ProbeFailure> {
    let json_output = Command::new(path)
        .arg("version")
        .arg("-json")
        .output()
        .map_err(|error| map_launch_error(path, error))?;

    if json_output.status.success() {
        if let Some(version) = parse_tofu_version_json(&json_output.stdout) {
            return Ok(version);
        }
    }

    let text_output = Command::new(path)
        .arg("version")
        .output()
        .map_err(|error| map_launch_error(path, error))?;

    if !text_output.status.success() {
        return Err(ProbeFailure {
            reason: TofuUnavailabilityReason::VersionProbeFailed,
            message: format!(
                "failed to probe tofu version at '{}': {}",
                path.display(),
                String::from_utf8_lossy(&text_output.stderr).trim()
            ),
        });
    }

    parse_tofu_version_text(&text_output.stdout).ok_or_else(|| ProbeFailure {
        reason: TofuUnavailabilityReason::VersionProbeFailed,
        message: format!(
            "failed to parse tofu version output from '{}'",
            path.display()
        ),
    })
}

fn map_launch_error(path: &Path, error: std::io::Error) -> ProbeFailure {
    let reason = match error.kind() {
        std::io::ErrorKind::PermissionDenied => TofuUnavailabilityReason::NotExecutable,
        _ => TofuUnavailabilityReason::VersionProbeFailed,
    };

    ProbeFailure {
        reason,
        message: format!("failed to execute tofu at '{}': {error}", path.display()),
    }
}

fn parse_tofu_version_json(stdout: &[u8]) -> Option<String> {
    let value = serde_json::from_slice::<Value>(stdout).ok()?;
    for key in [
        "terraform_version",
        "opentofu_version",
        "tofu_version",
        "version",
    ] {
        if let Some(version) = value.get(key).and_then(Value::as_str) {
            return Some(version.to_string());
        }
    }

    None
}

fn parse_tofu_version_text(stdout: &[u8]) -> Option<String> {
    let line = String::from_utf8_lossy(stdout)
        .lines()
        .next()?
        .trim()
        .to_string();
    let version = line.split_whitespace().find(|token| {
        token.starts_with('v') || token.chars().next().is_some_and(|c| c.is_ascii_digit())
    })?;

    Some(version.trim_start_matches('v').to_string())
}

fn is_supported_tofu_version(version: &str) -> bool {
    let core = version.split_once('-').map_or(version, |(core, _)| core);
    let mut components = core.split('.');
    let major = components
        .next()
        .and_then(|value| value.parse::<u64>().ok());
    let minor = components
        .next()
        .and_then(|value| value.parse::<u64>().ok());
    let patch = components
        .next()
        .and_then(|value| value.parse::<u64>().ok());

    matches!((major, minor, patch), (Some(1), Some(8..), Some(_)))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    use tempfile::TempDir;

    use super::*;

    static TOFU_BINARY_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn resolves_override_before_system() {
        let _guard = TOFU_BINARY_TEST_LOCK
            .lock()
            .expect("tofu binary test lock should be available");
        let binaries = TempDir::new().expect("temp dir should exist");
        let override_path =
            write_fake_tofu(binaries.path().join("override-tofu"), "1.11.5-override");
        let system_dir = binaries.path().join("system");
        fs::create_dir_all(&system_dir).expect("system dir should exist");
        write_fake_tofu(system_dir.join(DEFAULT_TOFU_BINARY_NAME), "1.11.5-system");

        let resolution = resolve_tofu(&TofuResolutionRequest {
            override_path: Some(override_path.clone()),
            system_search_paths: Some(vec![system_dir]),
            ..TofuResolutionRequest::default()
        })
        .expect("override should resolve first");

        assert_eq!(resolution.source, TofuSourceKind::Override);
        assert_eq!(resolution.path, override_path);
        assert_eq!(resolution.version, "1.11.5-override");
    }

    #[test]
    fn falls_back_to_system_when_override_missing() {
        let _guard = TOFU_BINARY_TEST_LOCK
            .lock()
            .expect("tofu binary test lock should be available");
        let binaries = TempDir::new().expect("temp dir should exist");
        let system_dir = binaries.path().join("bin");
        fs::create_dir_all(&system_dir).expect("system dir should exist");
        let system_path = write_fake_tofu(system_dir.join(DEFAULT_TOFU_BINARY_NAME), "1.11.5");

        let report = inspect_tofu_resolution(&TofuResolutionRequest {
            system_search_paths: Some(vec![system_dir]),
            ..TofuResolutionRequest::default()
        });

        assert_eq!(report.attempts.len(), 4);
        assert_eq!(
            report.selected.as_ref().map(|value| value.source),
            Some(TofuSourceKind::System)
        );
        assert_eq!(
            report.selected.as_ref().map(|value| value.path.clone()),
            Some(system_path)
        );
        assert!(matches!(
            report.attempts[0].outcome,
            TofuAttemptOutcome::Unavailable(TofuUnavailability {
                reason: TofuUnavailabilityReason::NotConfigured,
                ..
            })
        ));
    }

    #[test]
    fn reports_failure_when_no_sources_resolve() {
        let error = resolve_tofu(&TofuResolutionRequest {
            policy: TofuResolutionPolicy {
                preferred_sources: vec![TofuSourceKind::Override, TofuSourceKind::System],
                system_fallback_allowed: true,
            },
            system_search_paths: Some(Vec::new()),
            ..TofuResolutionRequest::default()
        })
        .expect_err("resolution should fail when no sources resolve");

        assert_eq!(error.attempts.len(), 2);
        assert!(matches!(
            error.attempts[1].outcome,
            TofuAttemptOutcome::Unavailable(TofuUnavailability {
                reason: TofuUnavailabilityReason::NotFound,
                ..
            })
        ));
    }

    #[test]
    fn enforces_version_requirement() {
        let _guard = TOFU_BINARY_TEST_LOCK
            .lock()
            .expect("tofu binary test lock should be available");
        let binaries = TempDir::new().expect("temp dir should exist");
        let system_dir = binaries.path().join("bin");
        fs::create_dir_all(&system_dir).expect("system dir should exist");
        write_fake_tofu(system_dir.join(DEFAULT_TOFU_BINARY_NAME), "1.10.0");

        let error = resolve_tofu(&TofuResolutionRequest {
            system_search_paths: Some(vec![system_dir]),
            version_requirement: Some(TofuVersionRequirement {
                pinned_version: "1.11.5".to_string(),
            }),
            policy: TofuResolutionPolicy {
                preferred_sources: vec![TofuSourceKind::System],
                system_fallback_allowed: true,
            },
            ..TofuResolutionRequest::default()
        })
        .expect_err("mismatched version should fail resolution");

        assert!(matches!(
            error.attempts[0].outcome,
            TofuAttemptOutcome::Unavailable(TofuUnavailability {
                reason: TofuUnavailabilityReason::VersionMismatch,
                ..
            })
        ));
    }

    #[test]
    fn enforces_the_supported_tofu_version_range_boundaries() {
        assert!(!is_supported_tofu_version("1.7.9"));
        assert!(is_supported_tofu_version("1.8.0"));
        assert!(is_supported_tofu_version("1.11.5"));
        assert!(!is_supported_tofu_version("2.0.0"));
    }

    fn write_fake_tofu(path: PathBuf, version: &str) -> PathBuf {
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"version\" ] && [ \"$2\" = \"-json\" ]; then\n  printf '{{\"terraform_version\":\"{version}\"}}\\n'\n  exit 0\nfi\nif [ \"$1\" = \"version\" ]; then\n  printf 'OpenTofu v{version}\\n'\n  exit 0\nfi\nexit 1\n"
        );

        fs::write(&path, script).expect("script should be written");
        let mut permissions = fs::metadata(&path)
            .expect("metadata should exist")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("permissions should be updated");
        path
    }
}
