use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result as AnyhowResult};
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct YaffleConfig {
    pub version: u8,
    pub environments: Vec<Environment>,
    pub workspaces: Vec<Workspace>,
    pub cloud: CloudConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Environment {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Workspace {
    pub path: String,
    pub environments: EnvironmentSelector,
    pub automatic_preview_isolation: bool,
    pub variables: BTreeMap<String, VariableValue>,
    pub outputs: BTreeMap<String, WorkspaceOutputPolicy>,
    pub activation: Vec<LifecycleHook>,
    pub verification: Vec<LifecycleHook>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentSelector {
    All,
    Named(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum VariableValue {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceOutputPolicy {
    pub visibility: OutputVisibility,
    pub consumers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputVisibility {
    Internal,
    Public,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleHook {
    pub key: String,
    pub environments: Vec<String>,
    pub kind: LifecycleHookKind,
    pub timeout: Option<String>,
    pub failure: LifecycleFailurePolicy,
    pub scopes: Vec<String>,
    pub dispatch: LifecycleHookDispatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleHookKind {
    Generic,
    GenericHmac,
    GitHubRepositoryDispatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleFailurePolicy {
    Failed,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleHookDispatch {
    Generic(LifecycleWebhookRequest),
    GitHubRepositoryDispatch(LifecycleGitHubRepositoryDispatch),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleWebhookRequest {
    pub url: String,
    pub method: String,
    pub auth: Option<LifecycleWebhookAuth>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleGitHubRepositoryDispatch {
    pub owner: Option<String>,
    pub repo: Option<String>,
    pub event_type: String,
    pub api_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleWebhookAuth {
    pub scheme: LifecycleWebhookAuthScheme,
    pub secret_ref: Option<String>,
    pub connection: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleWebhookAuthScheme {
    Bearer,
    HmacSha256,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CloudConfig {
    pub triggers: Triggers,
    pub approvals: Vec<Approval>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Approval {
    pub workspaces: Vec<String>,
    pub environments: Vec<String>,
    pub approvers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Triggers {
    pub github: Option<GitHubTriggers>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitHubTriggers {
    pub push: Vec<PushTrigger>,
    pub pull_request: Vec<PullRequestTrigger>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushTrigger {
    pub ref_patterns: Vec<String>,
    pub exclude_ref_patterns: Vec<String>,
    pub environment: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestTrigger {
    pub branch_patterns: Vec<String>,
    pub exclude_branch_patterns: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Failed to parse yaffle.toml: {0}")]
    ParseToml(String),
    #[error("Invalid yaffle.toml:\n{0}")]
    Validation(String),
}

pub fn validate_environment_name(environment: &str) -> AnyhowResult<()> {
    let environment = environment.trim();
    if environment.is_empty() {
        bail!("environment name must not be empty")
    }
    if environment == "."
        || environment == ".."
        || environment.contains('/')
        || environment.contains('\\')
        || !environment
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    {
        bail!("environment name must be one safe path segment using letters, digits, '.', '_', or '-'")
    }

    Ok(())
}

pub fn parse_yaffle_toml(input: &str) -> Result<YaffleConfig, ConfigError> {
    let raw: RawConfig =
        toml::from_str(input).map_err(|error| ConfigError::ParseToml(error.to_string()))?;
    normalize_and_validate(raw)
}

pub fn environment_name_matches_patterns(environment: &str, patterns: &[String]) -> bool {
    patterns
        .iter()
        .any(|pattern| wildcard_match(pattern, environment))
}

fn normalize_and_validate(raw: RawConfig) -> Result<YaffleConfig, ConfigError> {
    let mut errors = Vec::new();

    if raw.version != 1 {
        errors.push(format!("version: expected 1, got {}", raw.version));
    }

    if raw.triggers.is_some() {
        errors.push(
            "top-level triggers are no longer supported; move them under cloud.triggers"
                .to_string(),
        );
    }

    if raw.approvals.is_some() {
        errors.push(
            "top-level approvals are no longer supported; move them under cloud.approvals"
                .to_string(),
        );
    }

    if raw.workspaces.is_empty() {
        errors.push("workspaces: at least one workspace is required".to_string());
    }

    let environments = raw.environments.unwrap_or_default();
    let declared_environments: BTreeSet<String> = environments
        .iter()
        .map(|environment| environment.name.clone())
        .collect();
    if declared_environments.len() != environments.len() {
        errors.push("environments: duplicate environment names are not allowed".to_string());
    }

    let mut seen_paths = BTreeSet::new();
    let mut workspaces = Vec::new();

    for workspace in raw.workspaces {
        if !seen_paths.insert(workspace.path.clone()) {
            errors.push(format!(
                "workspaces: duplicate workspace path '{}'",
                workspace.path
            ));
        }

        let environments = normalize_environment_selector(workspace.environments);
        let path_segments: Vec<&str> = workspace.path.split('/').collect();
        if workspace.path != "."
            && (workspace.path.starts_with('/')
                || workspace.path.starts_with("./")
                || workspace.path.ends_with('/')
                || path_segments
                    .iter()
                    .any(|segment| segment.is_empty() || *segment == "." || *segment == ".."))
        {
            errors.push(format!(
                "workspaces.{}: path must be repository-relative and normalized (for example, \"infra/app\" or \".\")",
                workspace.path
            ));
        }
        if let Some(shared_resources) = &workspace.shared_resources {
            if shared_resources.is_empty() {
                errors.push(format!(
                    "workspaces.{}.shared_resources: per-resource shared exceptions are not allowed; move shared resources to an upstream named, external, or static workspace and consume them through authorized immutable outputs or a read-only data source",
                    workspace.path
                ));
            }
            for resource in shared_resources {
                errors.push(format!(
                    "workspaces.{}.shared_resources.{}: per-resource shared exceptions are not allowed; move {} to an upstream named, external, or static workspace and consume it through authorized immutable outputs or a read-only data source",
                    workspace.path, resource, resource
                ));
            }
        }
        if workspace.automatic_preview_isolation.unwrap_or(false)
            && environments != EnvironmentSelector::All
        {
            errors.push(format!(
                "workspaces.{}: automatic_preview_isolation requires environments = \"*\"",
                workspace.path
            ));
        }
        if let EnvironmentSelector::Named(names) = &environments {
            for environment in names {
                if !declared_environments.contains(environment) {
                    errors.push(format!(
                        "workspaces.{}: references undeclared environment '{}'",
                        workspace.path, environment
                    ));
                }
            }
        }

        let outputs = normalize_output_policies(&workspace.path, workspace.outputs, &mut errors);
        let activation = normalize_lifecycle_hooks(
            &workspace.path,
            "activation",
            workspace.activation,
            &mut errors,
        );
        let verification = normalize_lifecycle_hooks(
            &workspace.path,
            "verification",
            workspace.verification,
            &mut errors,
        );

        workspaces.push(Workspace {
            path: workspace.path,
            environments,
            automatic_preview_isolation: workspace.automatic_preview_isolation.unwrap_or(false),
            variables: workspace.variables.unwrap_or_default(),
            outputs,
            activation,
            verification,
        });
    }

    let cloud = raw.cloud;
    let trigger_root = cloud.as_ref().and_then(|cloud| cloud.triggers.clone());
    let approvals = cloud.and_then(|cloud| cloud.approvals).unwrap_or_default();

    for approval in &approvals {
        for approver in &approval.approvers {
            if !is_valid_approver(approver) {
                errors.push(format!(
                    "invalid approver format \"{approver}\": expected github:user:<username> or github:team:<org>/<team>"
                ));
            }
        }
    }

    let triggers = normalize_triggers(trigger_root, &declared_environments, &mut errors);

    if !errors.is_empty() {
        return Err(ConfigError::Validation(
            errors
                .into_iter()
                .map(|error| format!("  - {error}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }

    Ok(YaffleConfig {
        version: 1,
        environments,
        workspaces,
        cloud: CloudConfig {
            triggers,
            approvals,
        },
    })
}

fn is_valid_approver(value: &str) -> bool {
    if let Some(username) = value.strip_prefix("github:user:") {
        return !username.trim().is_empty() && !username.contains('/');
    }
    if let Some(team) = value.strip_prefix("github:team:") {
        let mut parts = team.split('/');
        return parts.next().is_some_and(|part| !part.trim().is_empty())
            && parts.next().is_some_and(|part| !part.trim().is_empty())
            && parts.next().is_none();
    }
    false
}

fn normalize_environment_selector(raw: RawEnvironmentSelector) -> EnvironmentSelector {
    match raw {
        RawEnvironmentSelector::String(value) if value == "*" => EnvironmentSelector::All,
        RawEnvironmentSelector::String(value) => EnvironmentSelector::Named(vec![value]),
        RawEnvironmentSelector::Array(values) if values.iter().any(|value| value == "*") => {
            EnvironmentSelector::All
        }
        RawEnvironmentSelector::Array(values) => EnvironmentSelector::Named(values),
    }
}

fn normalize_output_policies(
    workspace_path: &str,
    raw: Option<BTreeMap<String, RawWorkspaceOutputPolicy>>,
    errors: &mut Vec<String>,
) -> BTreeMap<String, WorkspaceOutputPolicy> {
    let mut policies = BTreeMap::new();

    for (name, policy) in raw.unwrap_or_default() {
        let visibility = match policy.visibility.as_str() {
            "internal" => OutputVisibility::Internal,
            "public" => OutputVisibility::Public,
            other => {
                errors.push(format!(
                    "workspaces.{workspace_path}.outputs.{name}: invalid visibility '{other}'",
                ));
                continue;
            }
        };

        let consumers = policy.consumers.unwrap_or_default();
        if visibility == OutputVisibility::Public && consumers.is_empty() {
            errors.push(format!(
                "workspaces.{workspace_path}.outputs.{name}: public outputs must declare at least one consumer",
            ));
        }
        if visibility == OutputVisibility::Internal && !consumers.is_empty() {
            errors.push(format!(
                "workspaces.{workspace_path}.outputs.{name}: internal outputs cannot declare consumers",
            ));
        }

        policies.insert(
            name,
            WorkspaceOutputPolicy {
                visibility,
                consumers,
            },
        );
    }

    policies
}

fn normalize_lifecycle_hooks(
    workspace_path: &str,
    phase: &str,
    raw: Option<Vec<RawLifecycleHook>>,
    errors: &mut Vec<String>,
) -> Vec<LifecycleHook> {
    let mut hooks = Vec::new();
    let mut seen_keys = BTreeSet::new();

    for hook in raw.unwrap_or_default() {
        if hook.key.trim().is_empty() {
            errors.push(format!(
                "workspaces.{workspace_path}.{phase}: lifecycle hook key must not be empty",
            ));
            continue;
        }
        if !seen_keys.insert(hook.key.clone()) {
            errors.push(format!(
                "workspaces.{workspace_path}.{phase}: duplicate lifecycle hook key '{}'",
                hook.key,
            ));
            continue;
        }

        let kind = match hook.kind.as_str() {
            "webhook" | "generic" => LifecycleHookKind::Generic,
            "generic_hmac" => LifecycleHookKind::GenericHmac,
            "github_repository_dispatch" => LifecycleHookKind::GitHubRepositoryDispatch,
            other => {
                errors.push(format!(
                    "workspaces.{workspace_path}.{phase}.{}: invalid kind '{other}'",
                    hook.key,
                ));
                continue;
            }
        };

        let environments = hook.environments.unwrap_or_else(|| vec!["*".to_string()]);
        if environments.is_empty() || environments.iter().any(|value| value.trim().is_empty()) {
            errors.push(format!(
                "workspaces.{workspace_path}.{phase}.{}: environments must contain at least one non-empty pattern",
                hook.key,
            ));
            continue;
        }

        let failure = match hook
            .failure
            .unwrap_or_else(|| "failed".to_string())
            .as_str()
        {
            "failed" => LifecycleFailurePolicy::Failed,
            "degraded" => LifecycleFailurePolicy::Degraded,
            other => {
                errors.push(format!(
                    "workspaces.{workspace_path}.{phase}.{}: invalid failure policy '{other}'",
                    hook.key,
                ));
                continue;
            }
        };

        let scopes = hook.scopes.unwrap_or_else(|| match phase {
            "activation" => vec!["usable".to_string(), "acceptable".to_string()],
            "verification" => vec!["acceptable".to_string()],
            _ => Vec::new(),
        });
        if scopes.is_empty() {
            errors.push(format!(
                "workspaces.{workspace_path}.{phase}.{}: scopes must not be empty",
                hook.key,
            ));
            continue;
        }

        let dispatch = match kind {
            LifecycleHookKind::Generic | LifecycleHookKind::GenericHmac => {
                if hook.github.is_some() {
                    errors.push(format!(
                        "workspaces.{workspace_path}.{phase}.{}: github dispatch settings are only valid for kind 'github_repository_dispatch'",
                        hook.key,
                    ));
                    continue;
                }

                let request = match hook.request {
                    Some(request) => request,
                    None => {
                        errors.push(format!(
                            "workspaces.{workspace_path}.{phase}.{}: request is required",
                            hook.key,
                        ));
                        continue;
                    }
                };
                if request.url.trim().is_empty() {
                    errors.push(format!(
                        "workspaces.{workspace_path}.{phase}.{}: request.url must not be empty",
                        hook.key,
                    ));
                    continue;
                }
                let method = request.method.unwrap_or_else(|| "POST".to_string());
                if method != "POST" {
                    errors.push(format!(
                        "workspaces.{workspace_path}.{phase}.{}: only POST lifecycle webhooks are currently supported",
                        hook.key,
                    ));
                    continue;
                }

                let auth = match request.auth {
                    Some(auth) => {
                        if auth
                            .secret_ref
                            .as_deref()
                            .is_some_and(|value| value.trim().is_empty())
                        {
                            errors.push(format!(
                                "workspaces.{workspace_path}.{phase}.{}: auth.secret_ref must not be empty",
                                hook.key,
                            ));
                            continue;
                        }
                        if auth
                            .connection
                            .as_deref()
                            .is_some_and(|value| value.trim().is_empty())
                        {
                            errors.push(format!(
                                "workspaces.{workspace_path}.{phase}.{}: auth.connection must not be empty",
                                hook.key,
                            ));
                            continue;
                        }
                        let secret_ref = auth.secret_ref.filter(|value| !value.trim().is_empty());
                        let connection = auth.connection.filter(|value| !value.trim().is_empty());
                        if secret_ref.is_some() == connection.is_some() {
                            errors.push(format!(
                                "workspaces.{workspace_path}.{phase}.{}: auth must set exactly one of secret_ref or connection",
                                hook.key,
                            ));
                            continue;
                        }

                        let scheme_name = auth.scheme.unwrap_or_else(|| match kind {
                            LifecycleHookKind::Generic => "bearer".to_string(),
                            LifecycleHookKind::GenericHmac => "hmac_sha256".to_string(),
                            LifecycleHookKind::GitHubRepositoryDispatch => unreachable!(),
                        });
                        let scheme = match scheme_name.as_str() {
                            "bearer" => LifecycleWebhookAuthScheme::Bearer,
                            "hmac_sha256" => LifecycleWebhookAuthScheme::HmacSha256,
                            other => {
                                errors.push(format!(
                                    "workspaces.{workspace_path}.{phase}.{}: invalid auth scheme '{other}'",
                                    hook.key,
                                ));
                                continue;
                            }
                        };

                        match (kind, scheme) {
                            (
                                LifecycleHookKind::Generic,
                                LifecycleWebhookAuthScheme::HmacSha256,
                            ) => {
                                errors.push(format!(
                                    "workspaces.{workspace_path}.{phase}.{}: use kind 'generic_hmac' for HMAC-signed lifecycle hooks",
                                    hook.key,
                                ));
                                continue;
                            }
                            (
                                LifecycleHookKind::GenericHmac,
                                LifecycleWebhookAuthScheme::Bearer,
                            ) => {
                                errors.push(format!(
                                    "workspaces.{workspace_path}.{phase}.{}: kind 'generic_hmac' requires hmac_sha256 auth",
                                    hook.key,
                                ));
                                continue;
                            }
                            _ => {}
                        }

                        Some(LifecycleWebhookAuth {
                            scheme,
                            secret_ref,
                            connection,
                        })
                    }
                    None => {
                        if kind == LifecycleHookKind::GenericHmac {
                            errors.push(format!(
                                "workspaces.{workspace_path}.{phase}.{}: kind 'generic_hmac' requires request.auth with secret_ref or connection",
                                hook.key,
                            ));
                            continue;
                        }
                        None
                    }
                };

                LifecycleHookDispatch::Generic(LifecycleWebhookRequest {
                    url: request.url,
                    method,
                    auth,
                })
            }
            LifecycleHookKind::GitHubRepositoryDispatch => {
                if hook.request.is_some() {
                    errors.push(format!(
                        "workspaces.{workspace_path}.{phase}.{}: request is not used for kind 'github_repository_dispatch'",
                        hook.key,
                    ));
                    continue;
                }

                let github = match hook.github {
                    Some(github) => github,
                    None => {
                        errors.push(format!(
                            "workspaces.{workspace_path}.{phase}.{}: github settings are required for kind 'github_repository_dispatch'",
                            hook.key,
                        ));
                        continue;
                    }
                };
                if github.event_type.trim().is_empty() {
                    errors.push(format!(
                        "workspaces.{workspace_path}.{phase}.{}: github.event_type must not be empty",
                        hook.key,
                    ));
                    continue;
                }
                if github
                    .owner
                    .as_deref()
                    .is_some_and(|value| value.trim().is_empty())
                {
                    errors.push(format!(
                        "workspaces.{workspace_path}.{phase}.{}: github.owner must not be empty when provided",
                        hook.key,
                    ));
                    continue;
                }
                if github
                    .repo
                    .as_deref()
                    .is_some_and(|value| value.trim().is_empty())
                {
                    errors.push(format!(
                        "workspaces.{workspace_path}.{phase}.{}: github.repo must not be empty when provided",
                        hook.key,
                    ));
                    continue;
                }
                if github
                    .api_url
                    .as_deref()
                    .is_some_and(|value| value.trim().is_empty())
                {
                    errors.push(format!(
                        "workspaces.{workspace_path}.{phase}.{}: github.api_url must not be empty when provided",
                        hook.key,
                    ));
                    continue;
                }

                LifecycleHookDispatch::GitHubRepositoryDispatch(LifecycleGitHubRepositoryDispatch {
                    owner: github.owner,
                    repo: github.repo,
                    event_type: github.event_type,
                    api_url: github.api_url,
                })
            }
        };

        hooks.push(LifecycleHook {
            key: hook.key,
            environments,
            kind,
            timeout: hook.timeout,
            failure,
            scopes,
            dispatch,
        });
    }

    hooks
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    let parts = pattern.split('*').collect::<Vec<_>>();
    if parts.len() == 1 {
        return pattern == value;
    }

    let mut remainder = value;
    let starts_with_wildcard = pattern.starts_with('*');
    let ends_with_wildcard = pattern.ends_with('*');

    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }

        if index == 0 && !starts_with_wildcard {
            if !remainder.starts_with(part) {
                return false;
            }
            remainder = &remainder[part.len()..];
            continue;
        }

        if index == parts.len() - 1 && !ends_with_wildcard {
            return remainder.ends_with(part);
        }

        if let Some(position) = remainder.find(part) {
            remainder = &remainder[position + part.len()..];
        } else {
            return false;
        }
    }

    true
}

fn normalize_triggers(
    raw: Option<RawTriggers>,
    declared_environments: &BTreeSet<String>,
    errors: &mut Vec<String>,
) -> Triggers {
    let Some(raw) = raw else {
        return Triggers::default();
    };

    let github = raw.github.map(|github| {
        let push = github
            .push
            .unwrap_or_default()
            .into_iter()
            .filter_map(|trigger| {
                let ref_patterns = match (trigger.ref_, trigger.ref_patterns) {
                    (Some(single), None) => vec![single],
                    (None, Some(patterns)) => patterns,
                    (Some(_), Some(_)) => {
                        errors.push("cloud.triggers.github.push: ref and ref_patterns cannot both be set".to_string());
                        return None;
                    }
                    (None, None) => {
                        errors.push("cloud.triggers.github.push: ref or ref_patterns is required".to_string());
                        return None;
                    }
                };

                if !declared_environments.contains(&trigger.environment) {
                    errors.push(format!(
                        "cloud.triggers.github.push: references undeclared environment '{}'",
                        trigger.environment
                    ));
                }

                Some(PushTrigger {
                    ref_patterns,
                    exclude_ref_patterns: trigger.exclude_ref_patterns.unwrap_or_default(),
                    environment: trigger.environment,
                })
            })
            .collect();

        let pull_request = github
            .pull_request
            .unwrap_or_default()
            .into_iter()
            .filter_map(|trigger| {
                let branch_patterns = match (trigger.branch_pattern, trigger.branch_patterns) {
                    (Some(single), None) => vec![single],
                    (None, Some(patterns)) => patterns,
                    (Some(_), Some(_)) => {
                        errors.push(
                            "cloud.triggers.github.pull_request: branch_pattern and branch_patterns cannot both be set"
                                .to_string(),
                        );
                        return None;
                    }
                    (None, None) => {
                        errors.push(
                            "cloud.triggers.github.pull_request: branch_pattern or branch_patterns is required"
                                .to_string(),
                        );
                        return None;
                    }
                };

                Some(PullRequestTrigger {
                    branch_patterns,
                    exclude_branch_patterns: trigger.exclude_branch_patterns.unwrap_or_default(),
                })
            })
            .collect();

        GitHubTriggers { push, pull_request }
    });

    Triggers { github }
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    version: u8,
    environments: Option<Vec<Environment>>,
    workspaces: Vec<RawWorkspace>,
    cloud: Option<RawCloud>,
    triggers: Option<RawTriggers>,
    approvals: Option<Vec<Approval>>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawCloud {
    triggers: Option<RawTriggers>,
    approvals: Option<Vec<Approval>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWorkspace {
    path: String,
    environments: RawEnvironmentSelector,
    automatic_preview_isolation: Option<bool>,
    shared_resources: Option<Vec<String>>,
    variables: Option<BTreeMap<String, VariableValue>>,
    outputs: Option<BTreeMap<String, RawWorkspaceOutputPolicy>>,
    activation: Option<Vec<RawLifecycleHook>>,
    verification: Option<Vec<RawLifecycleHook>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawEnvironmentSelector {
    String(String),
    Array(Vec<String>),
}

#[derive(Debug, Deserialize)]
struct RawWorkspaceOutputPolicy {
    visibility: String,
    consumers: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct RawLifecycleHook {
    key: String,
    environments: Option<Vec<String>>,
    kind: String,
    timeout: Option<String>,
    failure: Option<String>,
    scopes: Option<Vec<String>>,
    request: Option<RawLifecycleWebhookRequest>,
    github: Option<RawLifecycleGitHubRepositoryDispatch>,
}

#[derive(Debug, Deserialize)]
struct RawLifecycleWebhookRequest {
    url: String,
    method: Option<String>,
    auth: Option<RawLifecycleWebhookAuth>,
}

#[derive(Debug, Deserialize)]
struct RawLifecycleWebhookAuth {
    scheme: Option<String>,
    secret_ref: Option<String>,
    connection: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawLifecycleGitHubRepositoryDispatch {
    owner: Option<String>,
    repo: Option<String>,
    event_type: String,
    api_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawTriggers {
    github: Option<RawGitHubTriggers>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawGitHubTriggers {
    push: Option<Vec<RawPushTrigger>>,
    pull_request: Option<Vec<RawPullRequestTrigger>>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawPushTrigger {
    #[serde(rename = "ref")]
    ref_: Option<String>,
    ref_patterns: Option<Vec<String>>,
    exclude_ref_patterns: Option<Vec<String>>,
    environment: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RawPullRequestTrigger {
    branch_pattern: Option<String>,
    branch_patterns: Option<Vec<String>>,
    exclude_branch_patterns: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repository_fixture() {
        let input = include_str!("../../../testdata/config/repository.toml");
        let config = parse_yaffle_toml(input).expect("repository fixture should parse");

        assert_eq!(config.version, 1);
        assert_eq!(config.environments[0].name, "main");
        assert!(!config.workspaces.is_empty());
    }

    #[test]
    fn parses_public_example_fixture() {
        let input = include_str!("../../../testdata/config/public-example.toml");
        let config = parse_yaffle_toml(input).expect("public example fixture should parse");

        assert_eq!(config.version, 1);
        assert!(!config.workspaces.is_empty());
        assert!(config.cloud.triggers.github.is_some());
    }

    #[test]
    fn parses_cloud_namespaced_triggers() {
        let input = r#"
version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "infra/app"
environments = ["main"]

[cloud]

[[cloud.triggers.github.push]]
ref_patterns = ["refs/heads/main"]
environment = "main"

[[cloud.triggers.github.pull_request]]
branch_patterns = ["*"]
"#;

        let config = parse_yaffle_toml(input).expect("config should parse");
        let github = config
            .cloud
            .triggers
            .github
            .expect("github triggers should exist");
        assert_eq!(github.push.len(), 1);
        assert_eq!(github.pull_request.len(), 1);
    }

    #[test]
    fn parses_automatic_preview_isolation_as_an_explicit_workspace_opt_in() {
        let input = r#"
version = 1

[[workspaces]]
path = "infra/app"
environments = ["*"]
automatic_preview_isolation = true
"#;

        let config = parse_yaffle_toml(input).expect("config should parse");
        assert!(config.workspaces[0].automatic_preview_isolation);
    }

    #[test]
    fn defaults_automatic_preview_isolation_to_false() {
        let input = r#"
version = 1

[[workspaces]]
path = "infra/app"
environments = ["*"]
"#;

        let config = parse_yaffle_toml(input).expect("config should parse");
        assert!(!config.workspaces[0].automatic_preview_isolation);
    }

    #[test]
    fn rejects_automatic_preview_isolation_on_a_named_only_workspace() {
        let input = r#"
version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "infra/app"
environments = ["main"]
automatic_preview_isolation = true
"#;

        let error = parse_yaffle_toml(input).expect_err("named-only opt-in should fail");
        assert!(error
            .to_string()
            .contains("automatic_preview_isolation requires environments = \"*\""));
    }

    #[test]
    fn rejects_misspelled_automatic_preview_isolation_fields() {
        let input = r#"
version = 1

[[workspaces]]
path = "infra/app"
environments = ["*"]
automatic_preveiw_isolation = true
"#;

        let error = parse_yaffle_toml(input).expect_err("unknown workspace fields should fail");
        assert!(error.to_string().contains("automatic_preveiw_isolation"));
    }

    #[test]
    fn rejects_per_resource_shared_exceptions() {
        let input = r#"
version = 1

[[workspaces]]
path = "infra/app"
environments = ["*"]
automatic_preview_isolation = true
shared_resources = ["aws_vpc.shared"]
"#;

        let error = parse_yaffle_toml(input).expect_err("shared exceptions should fail");
        let message = error.to_string();
        assert!(message.contains("shared_resources.aws_vpc.shared"));
        assert!(message.contains("upstream named, external, or static workspace"));
    }

    #[test]
    fn rejects_non_canonical_workspace_paths() {
        for path in ["./infra", "infra/", "infra/../shared"] {
            let input = format!(
                r#"
version = 1

[[workspaces]]
path = "{path}"
environments = ["*"]
automatic_preview_isolation = true
"#
            );

            let error = parse_yaffle_toml(&input).expect_err("non-canonical path should fail");
            assert!(error
                .to_string()
                .contains("path must be repository-relative and normalized"));
        }
    }

    #[test]
    fn parses_cloud_namespaced_approvals() {
        let input = r#"
version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "infra/app"
environments = ["main"]

[cloud]

[[cloud.approvals]]
workspaces = ["infra/app"]
environments = ["main"]
approvers = ["github:user:alice"]
"#;

        let config = parse_yaffle_toml(input).expect("config should parse");
        assert_eq!(config.cloud.approvals.len(), 1);
        assert_eq!(
            config.cloud.approvals[0].approvers,
            vec!["github:user:alice"]
        );
    }

    #[test]
    fn rejects_empty_approval_identity() {
        let input = r#"
version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "infra"
environments = ["main"]

[[cloud.approvals]]
workspaces = ["infra"]
environments = ["main"]
approvers = [""]
"#;

        let error = parse_yaffle_toml(input).expect_err("empty approver should fail");
        assert!(error.to_string().contains("invalid approver format"));
    }

    #[test]
    fn rejects_legacy_top_level_triggers() {
        let input = r#"
version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "infra/app"
environments = ["main"]

[[triggers.github.push]]
ref_patterns = ["refs/heads/main"]
environment = "main"
"#;

        let error = parse_yaffle_toml(input).expect_err("legacy top-level triggers should fail");
        assert!(error
            .to_string()
            .contains("top-level triggers are no longer supported"));
    }

    #[test]
    fn rejects_legacy_top_level_approvals() {
        let input = r#"
version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "infra/app"
environments = ["main"]

[[approvals]]
workspaces = ["infra/app"]
environments = ["main"]
approvers = ["github:user:alice"]
"#;

        let error = parse_yaffle_toml(input).expect_err("legacy top-level approvals should fail");
        assert!(error
            .to_string()
            .contains("top-level approvals are no longer supported"));
    }

    #[test]
    fn rejects_both_legacy_and_cloud_triggers() {
        let input = r#"
version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "infra/app"
environments = ["main"]

[cloud]

[[cloud.triggers.github.push]]
ref_patterns = ["refs/heads/main"]
environment = "main"

[[triggers.github.push]]
ref_patterns = ["refs/heads/main"]
environment = "main"
"#;

        let error = parse_yaffle_toml(input).expect_err("mixed trigger syntax should fail");
        assert!(error
            .to_string()
            .contains("top-level triggers are no longer supported"));
    }

    #[test]
    fn parses_workspace_lifecycle_hooks() {
        let input = r#"
version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "apps/web/infra"
environments = ["main"]

  [[workspaces.activation]]
  key = "preview-ready"
  environments = ["pr-*", "main"]
  kind = "generic_hmac"
  timeout = "10m"
  failure = "degraded"
  scopes = ["usable", "acceptable"]

    [workspaces.activation.request]
    url = "http://localhost:8787/hooks/preview-ready"
    method = "POST"

    [workspaces.activation.request.auth]
    scheme = "hmac_sha256"
    secret_ref = "PREVIEW_READY_SECRET"

  [[workspaces.verification]]
  key = "smoke"
  kind = "webhook"

    [workspaces.verification.request]
    url = "https://ci.example.com/hooks/smoke"
"#;

        let config = parse_yaffle_toml(input).expect("config should parse");
        let workspace = &config.workspaces[0];
        assert_eq!(workspace.activation.len(), 1);
        assert_eq!(workspace.verification.len(), 1);
        assert_eq!(workspace.activation[0].environments, vec!["pr-*", "main"]);
        assert_eq!(workspace.activation[0].kind, LifecycleHookKind::GenericHmac);
        assert_eq!(
            workspace.activation[0].failure,
            LifecycleFailurePolicy::Degraded
        );
        assert_eq!(workspace.verification[0].scopes, vec!["acceptable"]);
        match &workspace.activation[0].dispatch {
            LifecycleHookDispatch::Generic(request) => {
                assert_eq!(
                    request.auth.as_ref().unwrap().scheme,
                    LifecycleWebhookAuthScheme::HmacSha256
                );
                assert_eq!(
                    request.auth.as_ref().unwrap().secret_ref.as_deref(),
                    Some("PREVIEW_READY_SECRET")
                );
                assert_eq!(request.auth.as_ref().unwrap().connection, None);
            }
            LifecycleHookDispatch::GitHubRepositoryDispatch(_) => {
                panic!("expected generic lifecycle dispatch")
            }
        }
    }

    #[test]
    fn parses_github_repository_dispatch_lifecycle_hooks() {
        let input = r#"
version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "apps/control-plane/infra"
environments = ["main"]

  [[workspaces.activation]]
  key = "control-plane"
  kind = "github_repository_dispatch"

    [workspaces.activation.github]
    event_type = "yaffle.activation"
"#;

        let config = parse_yaffle_toml(input).expect("config should parse");
        let workspace = &config.workspaces[0];
        let hook = &workspace.activation[0];

        assert_eq!(hook.kind, LifecycleHookKind::GitHubRepositoryDispatch);
        match &hook.dispatch {
            LifecycleHookDispatch::GitHubRepositoryDispatch(github) => {
                assert_eq!(github.event_type, "yaffle.activation");
                assert_eq!(github.owner, None);
                assert_eq!(github.repo, None);
            }
            LifecycleHookDispatch::Generic(_) => {
                panic!("expected github repository dispatch")
            }
        }
    }

    #[test]
    fn parses_connection_backed_lifecycle_auth() {
        let input = r#"
version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "apps/web/infra"
environments = ["main"]

  [[workspaces.activation]]
  key = "buildkite"
  kind = "generic"

    [workspaces.activation.request]
    url = "https://hooks.example.com/buildkite"

      [workspaces.activation.request.auth]
      connection = "buildkite-prod-webhook"
"#;

        let config = parse_yaffle_toml(input).expect("config should parse");
        let hook = &config.workspaces[0].activation[0];

        match &hook.dispatch {
            LifecycleHookDispatch::Generic(request) => {
                let auth = request.auth.as_ref().expect("auth should exist");
                assert_eq!(auth.scheme, LifecycleWebhookAuthScheme::Bearer);
                assert_eq!(auth.secret_ref, None);
                assert_eq!(auth.connection.as_deref(), Some("buildkite-prod-webhook"));
            }
            LifecycleHookDispatch::GitHubRepositoryDispatch(_) => {
                panic!("expected generic lifecycle dispatch")
            }
        }
    }

    #[test]
    fn environment_name_patterns_support_wildcards() {
        assert!(environment_name_matches_patterns(
            "pr-42",
            &["pr-*".to_string()]
        ));
        assert!(environment_name_matches_patterns(
            "main",
            &["main".to_string()]
        ));
        assert!(!environment_name_matches_patterns(
            "dev",
            &["main".to_string()]
        ));
    }

    #[test]
    fn rejects_environment_names_that_can_escape_state_storage() {
        for environment in ["../outside", "/tmp/outside", "nested/name", "..", "."] {
            assert!(
                validate_environment_name(environment).is_err(),
                "{environment} should be rejected"
            );
        }

        validate_environment_name("pr-42").expect("canonical transient name should be valid");
        validate_environment_name("production.us-east-1")
            .expect("safe named environment should be valid");
    }
}
