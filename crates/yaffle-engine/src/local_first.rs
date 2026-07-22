use std::env;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(test)]
use std::sync::{LazyLock, Mutex};

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::redirect::Policy;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;

#[cfg(test)]
pub(crate) static LOCAL_FIRST_ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StoredPrincipalType {
    Account,
    #[default]
    AnonymousSession,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredPrincipalCredential {
    #[serde(default, alias = "principalType")]
    pub principal_type: StoredPrincipalType,
    #[serde(alias = "principalId")]
    pub principal_id: String,
    #[serde(default, alias = "sessionId")]
    pub session_id: Option<String>,
    pub token: String,
    #[serde(alias = "issuedAt")]
    pub issued_at: String,
    #[serde(alias = "expiresAt")]
    pub expires_at: Option<String>,
    #[serde(default, alias = "userId")]
    pub user_id: Option<String>,
    #[serde(default, alias = "userEmail")]
    pub user_email: Option<String>,
    #[serde(default, alias = "userName")]
    pub user_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCloudAuthStatus {
    pub auth_store_path: PathBuf,
    pub stored_principal: Option<StoredPrincipalCredential>,
    pub expired: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionCredential {
    pub token: String,
    #[serde(alias = "repoBindingId")]
    pub repo_binding_id: String,
    #[serde(alias = "expiresAt")]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HostedOutputModulePublishRequest<'a> {
    pub canonical_repo_namespace: &'a str,
    pub local_repo_fingerprint: &'a str,
    pub environment_name: &'a str,
    pub workspace_path: &'a str,
    pub selected_output_names: &'a [String],
    pub outputs: &'a serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostedOutputModulePublishResult {
    pub id: String,
    #[serde(alias = "repoBindingId")]
    pub repo_binding_id: String,
    #[serde(alias = "workspacePath")]
    pub workspace_path: String,
    #[serde(alias = "environmentName")]
    pub environment_name: String,
    #[serde(alias = "versionSerial")]
    pub version_serial: u64,
    pub version: String,
    #[serde(alias = "createdAt")]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudCliLoginResult {
    #[serde(flatten)]
    pub principal: StoredPrincipalCredential,
    #[serde(default, alias = "convertedFromAnonymous")]
    pub converted_from_anonymous: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CloudCliAuthorizeRequestResult {
    #[serde(alias = "authorizeUrl")]
    authorize_url: String,
    #[serde(default, alias = "expiresAt")]
    expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CloudCliAuthorizeRequestEnvelope {
    data: CloudCliAuthorizeRequestResult,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudCliCapabilities {
    #[serde(alias = "principalType")]
    pub principal_type: String,
    #[serde(alias = "repoFullName")]
    pub repo_full_name: String,
    #[serde(alias = "executionMode")]
    pub execution_mode: String,
    #[serde(alias = "principalTier")]
    pub principal_tier: String,
    #[serde(alias = "remoteConverge")]
    pub remote_converge: CloudRemoteConvergeCapability,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudRemoteConvergeCapability {
    pub available: bool,
    #[serde(alias = "reasonCode")]
    pub reason_code: Option<String>,
    pub message: String,
    #[serde(alias = "upgradeUrl")]
    pub upgrade_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudCliInventory {
    #[serde(alias = "repoFullName")]
    pub repo_full_name: String,
    pub environments: Vec<CloudCliInventoryEnvironment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudCliInventoryEnvironment {
    pub repo: String,
    #[serde(alias = "environmentKind")]
    pub environment_kind: String,
    #[serde(alias = "environmentName")]
    pub environment_name: String,
    #[serde(alias = "sourceKind")]
    pub source_kind: Option<String>,
    pub status: String,
    #[serde(alias = "ref")]
    pub git_ref: String,
    #[serde(alias = "headSha")]
    pub head_sha: String,
    #[serde(alias = "updatedAt")]
    pub updated_at: String,
    #[serde(alias = "workspaceCount")]
    pub workspace_count: usize,
    #[serde(default, alias = "statusVector")]
    pub status_vector: Vec<CloudCliInventoryStatusCount>,
    #[serde(alias = "activeRunGroupId")]
    pub active_run_group_id: Option<String>,
    #[serde(alias = "prNumber")]
    pub pr_number: Option<u64>,
    #[serde(alias = "actorLogin")]
    pub actor_login: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudCliInventoryStatusCount {
    pub status: String,
    pub count: usize,
}

#[derive(Debug, Clone)]
pub struct CloudRemoteConvergeRequest {
    pub repo_full_name: String,
    pub canonical_repo_namespace: String,
    pub local_repo_fingerprint: String,
    pub environment_name: String,
    pub git_ref: String,
    pub head_sha: String,
    pub workspace_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudRemoteConvergeHandle {
    #[serde(alias = "runGroupId")]
    pub run_group_id: String,
    #[serde(alias = "scanJobId")]
    pub scan_job_id: String,
    #[serde(alias = "environmentName")]
    pub environment_name: String,
    #[serde(alias = "workspacePaths")]
    pub workspace_paths: Vec<String>,
    #[serde(alias = "ref")]
    pub git_ref: String,
    #[serde(alias = "headSha")]
    pub head_sha: String,
    #[serde(alias = "webUrl")]
    pub web_url: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudRemoteRunGroupSummary {
    pub id: String,
    pub status: String,
    pub repo: String,
    #[serde(alias = "environmentKind")]
    pub environment_kind: String,
    #[serde(alias = "environmentName")]
    pub environment_name: String,
    #[serde(alias = "ref")]
    pub git_ref: String,
    #[serde(alias = "headSha")]
    pub head_sha: String,
    #[serde(alias = "selectedWorkspacePaths")]
    pub selected_workspace_paths: Vec<String>,
    pub trigger: String,
    #[serde(alias = "createdAt")]
    pub created_at: String,
    #[serde(alias = "startedAt")]
    pub started_at: Option<String>,
    #[serde(alias = "completedAt")]
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudRemoteLatestRunSummary {
    pub id: String,
    #[serde(alias = "runType")]
    pub run_type: String,
    pub status: String,
    #[serde(alias = "planSummary")]
    pub plan_summary: Option<String>,
    #[serde(alias = "errorMessage")]
    pub error_message: Option<String>,
    #[serde(alias = "logOutput")]
    pub log_output: Option<String>,
    #[serde(alias = "createdAt")]
    pub created_at: String,
    #[serde(alias = "startedAt")]
    pub started_at: Option<String>,
    #[serde(alias = "completedAt")]
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudRemoteDeploymentStatus {
    pub id: String,
    #[serde(alias = "workspacePath")]
    pub workspace_path: String,
    pub status: String,
    #[serde(alias = "latestRun")]
    pub latest_run: Option<CloudRemoteLatestRunSummary>,
    #[serde(default)]
    pub runs: Vec<CloudRemoteLatestRunSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudRemoteConvergeStatus {
    #[serde(alias = "runGroup")]
    pub run_group: CloudRemoteRunGroupSummary,
    pub deployments: Vec<CloudRemoteDeploymentStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LifecycleRunHandle {
    pub id: String,
    #[serde(alias = "repoBindingId")]
    pub repo_binding_id: String,
    #[serde(alias = "environmentName")]
    pub environment_name: String,
    #[serde(alias = "executionMode")]
    pub execution_mode: String,
    pub status: String,
    #[serde(alias = "startedAt")]
    pub started_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LifecycleItemHandle {
    pub id: String,
    pub state: String,
    #[serde(alias = "onCompletionUrl")]
    pub on_completion_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LifecycleEventSnapshot {
    pub id: String,
    #[serde(alias = "eventType")]
    pub event_type: String,
    #[serde(default)]
    pub payload: serde_json::Map<String, serde_json::Value>,
    #[serde(alias = "createdAt")]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LifecycleItemSnapshot {
    pub id: String,
    #[serde(alias = "workspacePath")]
    pub workspace_path: String,
    pub key: String,
    pub phase: String,
    pub state: String,
    #[serde(alias = "failurePolicy")]
    pub failure_policy: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    pub summary: Option<String>,
    pub reason: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Map<String, serde_json::Value>,
    #[serde(alias = "startedAt")]
    pub started_at: Option<String>,
    #[serde(alias = "finishedAt")]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub events: Vec<LifecycleEventSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LifecycleStateSnapshot {
    pub run: Option<LifecycleRunSummary>,
    #[serde(default)]
    pub items: Vec<LifecycleItemSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LifecycleRunSummary {
    pub id: String,
    pub status: String,
    #[serde(alias = "executionMode")]
    pub execution_mode: String,
    #[serde(alias = "startedAt")]
    pub started_at: String,
    #[serde(alias = "finishedAt")]
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LifecycleRunRequest<'a> {
    pub canonical_repo_namespace: &'a str,
    pub local_repo_fingerprint: &'a str,
    pub environment_name: &'a str,
    pub execution_mode: &'a str,
}

#[derive(Debug, Clone)]
pub struct LifecycleItemRequest<'a> {
    pub run_id: &'a str,
    pub workspace_path: &'a str,
    pub key: &'a str,
    pub phase: &'a str,
    pub failure_policy: &'a str,
    pub scopes: &'a [String],
    pub destination_url: &'a str,
    pub destination_class: &'a str,
    pub dispatch_mode: &'a str,
    pub selected_output_names: &'a [String],
    pub summary: Option<&'a str>,
    pub metadata: &'a serde_json::Map<String, serde_json::Value>,
    pub callback_ttl_minutes: u64,
}

#[derive(Debug, Clone)]
pub struct LifecycleAdmissionRequest<'a> {
    pub canonical_repo_namespace: &'a str,
    pub local_repo_fingerprint: &'a str,
    pub environment_name: &'a str,
    pub execution_mode: &'a str,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LifecycleAdmissionDecision {
    pub allowed: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExecutionCredentialRequest<'a> {
    pub canonical_repo_namespace: &'a str,
    pub local_repo_fingerprint: &'a str,
    pub environment_name: &'a str,
    pub consumer_workspace_path: &'a str,
    pub session_kind: ExecutionCredentialKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionCredentialKind {
    WorkspaceInit,
    ShellSession,
}

impl ExecutionCredentialKind {
    fn api_value(self) -> &'static str {
        match self {
            Self::WorkspaceInit => "workspace_init",
            Self::ShellSession => "shell_session",
        }
    }
}

#[derive(Debug, Error)]
pub enum LocalFirstError {
    #[error("{0}")]
    Config(String),
    #[error("failed to read local principal store: {0}")]
    ReadStore(String),
    #[error("failed to write local principal store: {0}")]
    WriteStore(String),
    #[error("failed to call local-first backend: {0}")]
    Http(String),
    #[error("local-first backend rejected the request: {0}")]
    Api(String),
}

impl LocalFirstError {
    pub fn friendly_message(&self) -> String {
        match self {
            LocalFirstError::Http(message) => format!(
                "Yaffle could not reach the local-first backend. Check that the control plane is running and that `YAFFLE_MODULE_API_HOST` points to the right local host.\n\nTransport detail: {message}"
            ),
            LocalFirstError::Api(message) => format!(
                "The local-first backend rejected this request.\n\nBackend detail: {message}"
            ),
            _ => self.to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct AnonymousSessionResponseEnvelope {
    data: StoredPrincipalCredential,
}

#[derive(Debug, Deserialize)]
struct CloudCliLoginResponseEnvelope {
    data: CloudCliLoginResult,
}

#[derive(Debug, Deserialize)]
struct CloudCliCapabilitiesResponseEnvelope {
    data: CloudCliCapabilities,
}

#[derive(Debug, Deserialize)]
struct CloudCliInventoryResponseEnvelope {
    data: CloudCliInventory,
}

#[derive(Debug, Deserialize)]
struct CloudRemoteConvergeHandleEnvelope {
    data: CloudRemoteConvergeHandle,
}

#[derive(Debug, Deserialize)]
struct CloudRemoteConvergeStatusEnvelope {
    data: CloudRemoteConvergeStatus,
}

#[derive(Debug, Deserialize)]
struct ExecutionCredentialResponseEnvelope {
    data: ExecutionCredential,
}

#[derive(Debug, Deserialize)]
struct HostedOutputModulePublishResponseEnvelope {
    data: HostedOutputModulePublishResult,
}

#[derive(Debug, Deserialize)]
struct LifecycleRunResponseEnvelope {
    data: LifecycleRunHandle,
}

#[derive(Debug, Deserialize)]
struct LifecycleItemResponseEnvelope {
    data: LifecycleItemHandle,
}

#[derive(Debug, Deserialize)]
struct LifecycleItemSnapshotResponseEnvelope {
    data: LifecycleItemSnapshot,
}

#[derive(Debug, Deserialize)]
struct LifecycleStateResponseEnvelope {
    data: Option<LifecycleStateSnapshot>,
}

#[derive(Debug, Deserialize)]
struct LifecycleAdmissionResponseEnvelope {
    data: LifecycleAdmissionDecision,
}

#[derive(Debug, Deserialize)]
struct ApiErrorEnvelope {
    error: ApiErrorBody,
}

#[derive(Debug, Deserialize)]
struct ApiErrorBody {
    code: String,
    message: String,
}

pub fn ensure_anonymous_principal() -> Result<StoredPrincipalCredential, LocalFirstError> {
    if let Some(stored) = load_stored_principal()? {
        if !principal_expired(&stored) {
            return Ok(stored);
        }
    }

    let runtime = LocalFirstRuntime::from_env()?;
    let response = runtime
        .client
        .post(runtime.endpoint_url("/api/sessions/anonymous"))
        .send()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;

    if !response.status().is_success() {
        return Err(LocalFirstError::Api(read_api_error(response)?));
    }

    let stored = response
        .json::<AnonymousSessionResponseEnvelope>()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?
        .data;
    persist_principal(&stored)?;
    Ok(stored)
}

pub fn mint_execution_credential(
    principal: &StoredPrincipalCredential,
    request: &ExecutionCredentialRequest<'_>,
) -> Result<ExecutionCredential, LocalFirstError> {
    let runtime = LocalFirstRuntime::from_env()?;
    let response = runtime
        .client
        .post(runtime.endpoint_url("/api/execution-tokens"))
        .headers(runtime.authorized_headers(&principal.token)?)
        .json(&serde_json::json!({
            "canonicalRepoNamespace": request.canonical_repo_namespace,
            "localRepoFingerprint": request.local_repo_fingerprint,
            "environmentName": request.environment_name,
            "consumerWorkspacePath": request.consumer_workspace_path,
            "sessionKind": request.session_kind.api_value(),
        }))
        .send()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;

    if !response.status().is_success() {
        return Err(LocalFirstError::Api(read_api_error(response)?));
    }

    response
        .json::<ExecutionCredentialResponseEnvelope>()
        .map(|value| value.data)
        .map_err(|error| LocalFirstError::Http(error.to_string()))
}

pub fn publish_hosted_output_module(
    principal: &StoredPrincipalCredential,
    request: &HostedOutputModulePublishRequest<'_>,
) -> Result<HostedOutputModulePublishResult, LocalFirstError> {
    let runtime = LocalFirstRuntime::from_env()?;
    let response = runtime
        .client
        .put(runtime.endpoint_url("/api/output-modules"))
        .headers(runtime.authorized_headers(&principal.token)?)
        .json(&serde_json::json!({
            "canonicalRepoNamespace": request.canonical_repo_namespace,
            "localRepoFingerprint": request.local_repo_fingerprint,
            "environmentName": request.environment_name,
            "workspacePath": request.workspace_path,
            "selectedOutputNames": request.selected_output_names,
            "outputs": request.outputs,
        }))
        .send()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;

    if !response.status().is_success() {
        return Err(LocalFirstError::Api(read_api_error(response)?));
    }

    response
        .json::<HostedOutputModulePublishResponseEnvelope>()
        .map(|value| value.data)
        .map_err(|error| LocalFirstError::Http(error.to_string()))
}

pub fn create_lifecycle_run(
    principal: &StoredPrincipalCredential,
    request: &LifecycleRunRequest<'_>,
) -> Result<LifecycleRunHandle, LocalFirstError> {
    let runtime = LocalFirstRuntime::from_env()?;
    let response = runtime
        .client
        .post(runtime.endpoint_url("/api/lifecycle/runs"))
        .headers(runtime.authorized_headers(&principal.token)?)
        .json(&serde_json::json!({
            "canonicalRepoNamespace": request.canonical_repo_namespace,
            "localRepoFingerprint": request.local_repo_fingerprint,
            "environmentName": request.environment_name,
            "executionMode": request.execution_mode,
        }))
        .send()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;

    if !response.status().is_success() {
        return Err(LocalFirstError::Api(read_api_error(response)?));
    }

    response
        .json::<LifecycleRunResponseEnvelope>()
        .map(|value| value.data)
        .map_err(|error| LocalFirstError::Http(error.to_string()))
}

pub fn check_lifecycle_admission(
    principal: &StoredPrincipalCredential,
    request: &LifecycleAdmissionRequest<'_>,
) -> Result<LifecycleAdmissionDecision, LocalFirstError> {
    let runtime = LocalFirstRuntime::from_env()?;
    let response = runtime
        .client
        .post(runtime.endpoint_url("/api/lifecycle/admission"))
        .headers(runtime.authorized_headers(&principal.token)?)
        .json(&serde_json::json!({
            "canonicalRepoNamespace": request.canonical_repo_namespace,
            "localRepoFingerprint": request.local_repo_fingerprint,
            "environmentName": request.environment_name,
            "executionMode": request.execution_mode,
        }))
        .send()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;

    if !response.status().is_success() {
        return Err(LocalFirstError::Api(read_api_error(response)?));
    }

    response
        .json::<LifecycleAdmissionResponseEnvelope>()
        .map(|value| value.data)
        .map_err(|error| LocalFirstError::Http(error.to_string()))
}

pub fn create_lifecycle_item(
    principal: &StoredPrincipalCredential,
    request: &LifecycleItemRequest<'_>,
) -> Result<LifecycleItemHandle, LocalFirstError> {
    let runtime = LocalFirstRuntime::from_env()?;
    let response = runtime
        .client
        .post(runtime.endpoint_url("/api/lifecycle/items"))
        .headers(runtime.authorized_headers(&principal.token)?)
        .json(&serde_json::json!({
            "runId": request.run_id,
            "workspacePath": request.workspace_path,
            "key": request.key,
            "phase": request.phase,
            "kind": "webhook",
            "failurePolicy": request.failure_policy,
            "scopes": request.scopes,
            "destinationUrl": request.destination_url,
            "destinationClass": request.destination_class,
            "dispatchMode": request.dispatch_mode,
            "selectedOutputNames": request.selected_output_names,
            "summary": request.summary,
            "metadata": request.metadata,
            "callbackTtlMinutes": request.callback_ttl_minutes,
        }))
        .send()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;

    if !response.status().is_success() {
        return Err(LocalFirstError::Api(read_api_error(response)?));
    }

    response
        .json::<LifecycleItemResponseEnvelope>()
        .map(|value| value.data)
        .map_err(|error| LocalFirstError::Http(error.to_string()))
}

pub fn get_lifecycle_item(
    principal: &StoredPrincipalCredential,
    item_id: &str,
) -> Result<LifecycleItemSnapshot, LocalFirstError> {
    let runtime = LocalFirstRuntime::from_env()?;
    let response = runtime
        .client
        .get(runtime.endpoint_url(&format!("/api/lifecycle/items/{item_id}")))
        .headers(runtime.authorized_headers(&principal.token)?)
        .send()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;

    if !response.status().is_success() {
        return Err(LocalFirstError::Api(read_api_error(response)?));
    }

    response
        .json::<LifecycleItemSnapshotResponseEnvelope>()
        .map(|value| value.data)
        .map_err(|error| LocalFirstError::Http(error.to_string()))
}

pub fn dispatch_lifecycle_via_control_plane(
    principal: &StoredPrincipalCredential,
    body: &serde_json::Value,
) -> Result<(), LocalFirstError> {
    let runtime = LocalFirstRuntime::from_env()?;
    let response = runtime
        .client
        .post(runtime.endpoint_url("/api/lifecycle/dispatch"))
        .headers(runtime.authorized_headers(&principal.token)?)
        .json(body)
        .send()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;

    if !response.status().is_success() {
        return Err(LocalFirstError::Api(read_api_error(response)?));
    }

    Ok(())
}

pub fn get_lifecycle_state(
    principal: &StoredPrincipalCredential,
    canonical_repo_namespace: &str,
    local_repo_fingerprint: &str,
    environment_name: &str,
) -> Result<Option<LifecycleStateSnapshot>, LocalFirstError> {
    let runtime = LocalFirstRuntime::from_env()?;
    let response = runtime
        .client
        .get(runtime.endpoint_url("/api/lifecycle/state"))
        .headers(runtime.authorized_headers(&principal.token)?)
        .query(&[
            ("canonicalRepoNamespace", canonical_repo_namespace),
            ("localRepoFingerprint", local_repo_fingerprint),
            ("environmentName", environment_name),
        ])
        .send()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;

    if !response.status().is_success() {
        return Err(LocalFirstError::Api(read_api_error(response)?));
    }

    response
        .json::<LifecycleStateResponseEnvelope>()
        .map(|value| value.data)
        .map_err(|error| LocalFirstError::Http(error.to_string()))
}

pub fn compute_local_repo_fingerprint(repo_root: &Path) -> Result<String, LocalFirstError> {
    let canonical = repo_root.canonicalize().map_err(|error| {
        LocalFirstError::Config(format!("failed to canonicalize repo path: {error}"))
    })?;
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub fn load_local_cloud_auth_status() -> Result<LocalCloudAuthStatus, LocalFirstError> {
    let auth_store_path = local_auth_store_path()?;
    let stored_principal = load_stored_principal()?;
    let expired = stored_principal
        .as_ref()
        .map(principal_expired)
        .unwrap_or(false);

    Ok(LocalCloudAuthStatus {
        auth_store_path,
        stored_principal,
        expired,
    })
}

pub fn clear_local_cloud_auth() -> Result<bool, LocalFirstError> {
    let path = local_auth_store_path()?;
    let execution_dir = path
        .parent()
        .expect("local auth store should have a parent")
        .join("execution");
    let mut removed = false;
    if path.exists() {
        fs::remove_file(&path).map_err(|error| LocalFirstError::WriteStore(error.to_string()))?;
        removed = true;
    }
    if execution_dir.exists() {
        fs::remove_dir_all(execution_dir)
            .map_err(|error| LocalFirstError::WriteStore(error.to_string()))?;
        removed = true;
    }

    Ok(removed)
}

pub fn local_auth_store_path() -> Result<PathBuf, LocalFirstError> {
    let home = env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
        LocalFirstError::Config("HOME must be set to persist local-first auth".to_string())
    })?;
    Ok(home.join(".yaffle/auth/principal.json"))
}

pub fn module_api_base_url() -> Result<String, LocalFirstError> {
    let host = env::var("YAFFLE_MODULE_API_HOST")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "yaffle.dev".to_string());

    if host.starts_with("http://") || host.starts_with("https://") {
        let url = Url::parse(&host).map_err(|error| LocalFirstError::Config(error.to_string()))?;
        if url.scheme() == "https" || (url.scheme() == "http" && is_loopback_host(&url)) {
            return Ok(host);
        }
        return Err(LocalFirstError::Config(
            "YAFFLE_MODULE_API_HOST must use HTTPS except for exact loopback hosts".to_string(),
        ));
    }

    if host == "localhost"
        || host.starts_with("localhost:")
        || host == "127.0.0.1"
        || host.starts_with("127.0.0.1:")
        || host == "[::1]"
        || host.starts_with("[::1]:")
    {
        return Ok(format!("http://{host}"));
    }

    Ok(format!("https://{host}"))
}

fn is_loopback_host(url: &Url) -> bool {
    matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
}

fn loopback_redirect_port(redirect_uri: &str) -> Result<u16, LocalFirstError> {
    let url = Url::parse(redirect_uri).map_err(|error| {
        LocalFirstError::Config(format!("invalid cloud login callback URL: {error}"))
    })?;

    if url.scheme() != "http" {
        return Err(LocalFirstError::Config(
            "cloud login callback URL must use http".to_string(),
        ));
    }

    let host = url.host_str().unwrap_or_default();
    if host != "localhost" && host != "127.0.0.1" {
        return Err(LocalFirstError::Config(
            "cloud login callback URL must use localhost".to_string(),
        ));
    }

    if url.path() != "/callback" {
        return Err(LocalFirstError::Config(
            "cloud login callback URL path must be /callback".to_string(),
        ));
    }

    let port = url.port().ok_or_else(|| {
        LocalFirstError::Config("cloud login callback URL must include a port".to_string())
    })?;
    if port < 1024 {
        return Err(LocalFirstError::Config(
            "cloud login callback port must be 1024-65535".to_string(),
        ));
    }

    Ok(port)
}

pub fn build_cloud_cli_authorize_url(
    redirect_uri: &str,
    code_challenge: &str,
    state: &str,
) -> Result<String, LocalFirstError> {
    let redirect_port = loopback_redirect_port(redirect_uri)?;
    let runtime = LocalFirstRuntime::public_client_from_env()?;
    let response = runtime
        .client
        .post(runtime.endpoint_url("/api/cloud/cli/authorize-requests"))
        .json(&serde_json::json!({
            "client_id": "yaffle-cli",
            "redirect_port": redirect_port,
            "response_type": "code",
            "code_challenge": code_challenge,
            "code_challenge_method": "S256",
            "state": state,
        }))
        .send()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;
    if !response.status().is_success() {
        return Err(LocalFirstError::Http(read_api_error(response)?));
    }

    let parsed = response
        .json::<CloudCliAuthorizeRequestEnvelope>()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;
    Ok(parsed.data.authorize_url)
}

pub fn exchange_cloud_cli_login_code(
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
    current_principal: Option<&StoredPrincipalCredential>,
) -> Result<CloudCliLoginResult, LocalFirstError> {
    let redirect_port = loopback_redirect_port(redirect_uri)?;
    let runtime = LocalFirstRuntime::public_client_from_env()?;
    let mut body = serde_json::Map::from_iter([
        (
            "grant_type".to_string(),
            serde_json::Value::String("authorization_code".to_string()),
        ),
        (
            "code".to_string(),
            serde_json::Value::String(code.to_string()),
        ),
        (
            "code_verifier".to_string(),
            serde_json::Value::String(code_verifier.to_string()),
        ),
        (
            "redirect_port".to_string(),
            serde_json::Value::Number(serde_json::Number::from(redirect_port)),
        ),
        (
            "client_id".to_string(),
            serde_json::Value::String("yaffle-cli".to_string()),
        ),
    ]);
    if let Some(principal) = current_principal {
        body.insert(
            "current_principal_token".to_string(),
            serde_json::Value::String(principal.token.clone()),
        );
    }
    let response = runtime
        .client
        .post(runtime.endpoint_url("/api/cloud/cli/token"))
        .json(&body)
        .send()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;

    if !response.status().is_success() {
        return Err(LocalFirstError::Api(read_api_error(response)?));
    }

    let login = response
        .json::<CloudCliLoginResponseEnvelope>()
        .map(|value| value.data)
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;
    persist_principal(&login.principal)?;
    Ok(login)
}

pub fn get_cloud_cli_capabilities(
    principal: &StoredPrincipalCredential,
    repo_full_name: &str,
) -> Result<CloudCliCapabilities, LocalFirstError> {
    let runtime = LocalFirstRuntime::public_client_from_env()?;
    let response = runtime
        .client
        .get(runtime.endpoint_url("/api/cloud/capabilities"))
        .headers(runtime.bearer_headers(&principal.token)?)
        .query(&[("repoFullName", repo_full_name)])
        .send()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;

    if !response.status().is_success() {
        return Err(LocalFirstError::Api(read_api_error(response)?));
    }

    response
        .json::<CloudCliCapabilitiesResponseEnvelope>()
        .map(|value| value.data)
        .map_err(|error| LocalFirstError::Http(error.to_string()))
}

pub fn get_cloud_cli_inventory(
    principal: &StoredPrincipalCredential,
    repo_full_name: &str,
) -> Result<CloudCliInventory, LocalFirstError> {
    let runtime = LocalFirstRuntime::public_client_from_env()?;
    let response = runtime
        .client
        .get(runtime.endpoint_url("/api/cloud/inventory"))
        .headers(runtime.bearer_headers(&principal.token)?)
        .query(&[("repoFullName", repo_full_name)])
        .send()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;

    if !response.status().is_success() {
        return Err(LocalFirstError::Api(read_api_error(response)?));
    }

    response
        .json::<CloudCliInventoryResponseEnvelope>()
        .map(|value| value.data)
        .map_err(|error| LocalFirstError::Http(error.to_string()))
}

pub fn start_cloud_remote_converge(
    principal: &StoredPrincipalCredential,
    request: &CloudRemoteConvergeRequest,
) -> Result<CloudRemoteConvergeHandle, LocalFirstError> {
    let runtime = LocalFirstRuntime::public_client_from_env()?;
    let response = runtime
        .client
        .post(runtime.endpoint_url("/api/cloud/converge"))
        .headers(runtime.bearer_headers(&principal.token)?)
        .json(&serde_json::json!({
            "repoFullName": request.repo_full_name,
            "canonicalRepoNamespace": request.canonical_repo_namespace,
            "localRepoFingerprint": request.local_repo_fingerprint,
            "environmentName": request.environment_name,
            "ref": request.git_ref,
            "headSha": request.head_sha,
            "workspacePaths": request.workspace_paths,
        }))
        .send()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;

    if !response.status().is_success() {
        return Err(LocalFirstError::Api(read_api_error(response)?));
    }

    response
        .json::<CloudRemoteConvergeHandleEnvelope>()
        .map(|value| value.data)
        .map_err(|error| LocalFirstError::Http(error.to_string()))
}

pub fn get_cloud_remote_converge_status(
    principal: &StoredPrincipalCredential,
    run_group_id: &str,
) -> Result<CloudRemoteConvergeStatus, LocalFirstError> {
    let runtime = LocalFirstRuntime::public_client_from_env()?;
    let response = runtime
        .client
        .get(runtime.endpoint_url(&format!("/api/cloud/converge/{run_group_id}")))
        .headers(runtime.bearer_headers(&principal.token)?)
        .send()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;

    if !response.status().is_success() {
        return Err(LocalFirstError::Api(read_api_error(response)?));
    }

    response
        .json::<CloudRemoteConvergeStatusEnvelope>()
        .map(|value| value.data)
        .map_err(|error| LocalFirstError::Http(error.to_string()))
}

fn load_stored_principal() -> Result<Option<StoredPrincipalCredential>, LocalFirstError> {
    let path = local_auth_store_path()?;
    if !path.is_file() {
        return Ok(None);
    }

    let content =
        fs::read_to_string(path).map_err(|error| LocalFirstError::ReadStore(error.to_string()))?;
    serde_json::from_str(&content)
        .map(Some)
        .map_err(|error| LocalFirstError::ReadStore(error.to_string()))
}

fn persist_principal(principal: &StoredPrincipalCredential) -> Result<(), LocalFirstError> {
    let path = local_auth_store_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| LocalFirstError::WriteStore(error.to_string()))?;
        #[cfg(unix)]
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|error| LocalFirstError::WriteStore(error.to_string()))?;
    }

    let content = serde_json::to_vec_pretty(principal)
        .map_err(|error| LocalFirstError::WriteStore(error.to_string()))?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&path)
        .map_err(|error| LocalFirstError::WriteStore(error.to_string()))?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| LocalFirstError::WriteStore(error.to_string()))?;
    file.write_all(&content)
        .and_then(|()| file.sync_all())
        .map_err(|error| LocalFirstError::WriteStore(error.to_string()))
}

fn principal_expired(principal: &StoredPrincipalCredential) -> bool {
    let Some(expires_at) = &principal.expires_at else {
        return false;
    };

    let Ok(parsed) = time::OffsetDateTime::parse(expires_at, &Rfc3339) else {
        return false;
    };

    parsed <= time::OffsetDateTime::now_utc()
}

struct LocalFirstRuntime {
    client: Client,
    base_url: String,
}

impl LocalFirstRuntime {
    fn from_env() -> Result<Self, LocalFirstError> {
        Self::build()
    }

    fn public_client_from_env() -> Result<Self, LocalFirstError> {
        Self::build()
    }

    fn build() -> Result<Self, LocalFirstError> {
        let base_url = module_api_base_url()?;
        let mut client_builder = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .redirect(Policy::none());
        if should_allow_insecure_localhost_tls(&base_url) {
            client_builder = client_builder.danger_accept_invalid_certs(true);
        }

        Ok(Self {
            client: client_builder
                .build()
                .map_err(|error| LocalFirstError::Config(error.to_string()))?,
            base_url,
        })
    }

    fn endpoint_url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    fn authorized_headers(
        &self,
        bearer_token: &str,
    ) -> Result<reqwest::header::HeaderMap, LocalFirstError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {bearer_token}")
                .parse::<HeaderValue>()
                .map_err(|error| LocalFirstError::Config(error.to_string()))?,
        );
        Ok(headers)
    }

    fn bearer_headers(
        &self,
        bearer_token: &str,
    ) -> Result<reqwest::header::HeaderMap, LocalFirstError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {bearer_token}")
                .parse::<HeaderValue>()
                .map_err(|error| LocalFirstError::Config(error.to_string()))?,
        );
        Ok(headers)
    }
}

fn should_allow_insecure_localhost_tls(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return false;
    };
    if url.scheme() != "https" {
        return false;
    }

    let Some(host) = url.host_str() else {
        return false;
    };

    host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
        || host == "yaffle.local"
        || host.ends_with(".local")
        || host.starts_with("localhost.")
        || host.ends_with(".localhost")
}

fn read_api_error(response: reqwest::blocking::Response) -> Result<String, LocalFirstError> {
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;
    if let Ok(parsed) = serde_json::from_str::<ApiErrorEnvelope>(&body) {
        return Ok(format!(
            "{}: {} ({})",
            parsed.error.code, parsed.error.message, status
        ));
    }

    Ok(format!("{} {}", status, body))
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::thread;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn ensures_anonymous_principal_and_mints_execution_token() {
        let _guard = LOCAL_FIRST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let temp_home = TempDir::new().expect("temp dir should exist");
        let server = TestServer::start();

        let previous_home = env::var_os("HOME");
        let previous_host = env::var_os("YAFFLE_MODULE_API_HOST");

        env::set_var("HOME", temp_home.path());
        env::set_var(
            "YAFFLE_MODULE_API_HOST",
            format!("http://{}", server.authority()),
        );

        let principal = ensure_anonymous_principal().expect("principal bootstrap should succeed");
        let execution = mint_execution_credential(
            &principal,
            &ExecutionCredentialRequest {
                canonical_repo_namespace: "test-org--fixture",
                local_repo_fingerprint: "repo-fingerprint-1",
                environment_name: "pr-42",
                consumer_workspace_path: "apps/web/infra",
                session_kind: ExecutionCredentialKind::WorkspaceInit,
            },
        )
        .expect("execution credential should mint");
        let shell_session_execution = mint_execution_credential(
            &principal,
            &ExecutionCredentialRequest {
                canonical_repo_namespace: "test-org--fixture",
                local_repo_fingerprint: "repo-fingerprint-1",
                environment_name: "pr-42",
                consumer_workspace_path: "apps/web/infra",
                session_kind: ExecutionCredentialKind::ShellSession,
            },
        )
        .expect("shell session credential should mint");
        let published = publish_hosted_output_module(
            &principal,
            &HostedOutputModulePublishRequest {
                canonical_repo_namespace: "test-org--fixture",
                local_repo_fingerprint: "repo-fingerprint-1",
                environment_name: "pr-42",
                workspace_path: "infra/shared",
                selected_output_names: &["service_name".to_string()],
                outputs: &serde_json::Map::from_iter([(
                    "service_name".to_string(),
                    serde_json::json!({
                        "value": "shared",
                        "type_name": "string",
                        "sensitive": false,
                    }),
                )]),
            },
        )
        .expect("hosted output module should publish");
        let stored_path = local_auth_store_path().expect("store path should resolve");

        restore_env("HOME", previous_home);
        restore_env("YAFFLE_MODULE_API_HOST", previous_host);

        assert_eq!(principal.principal_id, "principal-test");
        assert_eq!(
            principal.principal_type,
            StoredPrincipalType::AnonymousSession
        );
        assert_eq!(principal.session_id.as_deref(), Some("session-test"));
        assert_eq!(execution.repo_binding_id, "binding-test");
        assert_eq!(execution.token, "execution-token-test");
        assert_eq!(
            shell_session_execution.token,
            "execution-token-shell-session"
        );
        assert_eq!(published.version, "1.0.1");
        assert!(stored_path.ends_with(Path::new(".yaffle/auth/principal.json")));
        assert!(stored_path.is_file());
        #[cfg(unix)]
        {
            let file_mode = fs::metadata(&stored_path)
                .expect("stored credential metadata should exist")
                .permissions()
                .mode()
                & 0o777;
            let directory_mode =
                fs::metadata(stored_path.parent().expect("store should have parent"))
                    .expect("credential directory metadata should exist")
                    .permissions()
                    .mode()
                    & 0o777;
            assert_eq!(file_mode, 0o600);
            assert_eq!(directory_mode, 0o700);
        }
    }

    #[test]
    fn reports_and_clears_local_cloud_auth_status() {
        let _guard = LOCAL_FIRST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let temp_home = TempDir::new().expect("temp dir should exist");
        let previous_home = env::var_os("HOME");

        env::set_var("HOME", temp_home.path());
        persist_principal(&StoredPrincipalCredential {
            principal_type: StoredPrincipalType::AnonymousSession,
            principal_id: "principal-test".to_string(),
            session_id: Some("session-test".to_string()),
            token: "principal-token-test".to_string(),
            issued_at: "2026-04-28T00:00:00Z".to_string(),
            expires_at: Some("2030-04-28T00:00:00Z".to_string()),
            user_id: None,
            user_email: None,
            user_name: None,
        })
        .expect("principal should persist");

        let status = load_local_cloud_auth_status().expect("status should load");
        let execution_dir = status.auth_store_path.parent().unwrap().join("execution");
        fs::create_dir_all(&execution_dir).expect("execution credential directory should exist");
        fs::write(execution_dir.join("stale.tfrc.json"), "secret")
            .expect("execution credential should exist");
        assert!(!status.expired);
        assert_eq!(
            status.stored_principal,
            Some(StoredPrincipalCredential {
                principal_type: StoredPrincipalType::AnonymousSession,
                principal_id: "principal-test".to_string(),
                session_id: Some("session-test".to_string()),
                token: "principal-token-test".to_string(),
                issued_at: "2026-04-28T00:00:00Z".to_string(),
                expires_at: Some("2030-04-28T00:00:00Z".to_string()),
                user_id: None,
                user_email: None,
                user_name: None,
            })
        );
        assert!(clear_local_cloud_auth().expect("clear should succeed"));
        assert!(!status.auth_store_path.exists());
        assert!(!execution_dir.exists());

        restore_env("HOME", previous_home);
    }

    #[test]
    fn allows_insecure_tls_for_local_dev_hosts() {
        assert!(should_allow_insecure_localhost_tls(
            "https://localhost:6969"
        ));
        assert!(should_allow_insecure_localhost_tls(
            "https://yaffle.local:6969"
        ));
        assert!(should_allow_insecure_localhost_tls(
            "https://foo.local:6969"
        ));
        assert!(!should_allow_insecure_localhost_tls(
            "https://yaffle.tail66f312.ts.net:6969"
        ));
    }

    #[test]
    fn rejects_plaintext_non_loopback_api_hosts() {
        let _guard = LOCAL_FIRST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let previous_host = env::var_os("YAFFLE_MODULE_API_HOST");
        env::set_var("YAFFLE_MODULE_API_HOST", "http://api.example.com");

        let error = module_api_base_url().expect_err("plaintext remote host should be rejected");

        restore_env("YAFFLE_MODULE_API_HOST", previous_host);
        assert!(error.to_string().contains("must use HTTPS"));
    }

    #[test]
    fn cloud_cli_authorize_url_does_not_require_a_client_secret() {
        let _guard = LOCAL_FIRST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let server = TestServer::start();
        let previous_host = env::var_os("YAFFLE_MODULE_API_HOST");

        env::set_var(
            "YAFFLE_MODULE_API_HOST",
            format!("http://{}", server.authority()),
        );

        let url = build_cloud_cli_authorize_url(
            "http://localhost:10000/callback",
            &"c".repeat(43),
            "state-test",
        )
        .expect("authorize URL should build");

        restore_env("YAFFLE_MODULE_API_HOST", previous_host);

        assert_eq!(
            url,
            "https://yaffle.dev/api/cloud/cli/authorize?request=opaque-test"
        );
        assert!(!url.contains("localhost"));
        assert!(!url.contains("redirect_uri"));
        assert!(!url.contains("feature_token"));
        assert!(!url.contains("test-feature-token"));
    }

    #[test]
    fn cloud_cli_token_exchange_sends_redirect_port_without_a_client_secret() {
        let _guard = LOCAL_FIRST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let temp_home = TempDir::new().expect("temp dir should exist");
        let server = TestServer::start();
        let previous_home = env::var_os("HOME");
        let previous_host = env::var_os("YAFFLE_MODULE_API_HOST");

        env::set_var("HOME", temp_home.path());
        env::set_var(
            "YAFFLE_MODULE_API_HOST",
            format!("http://{}", server.authority()),
        );

        let login = exchange_cloud_cli_login_code(
            "code-test",
            &"v".repeat(43),
            "http://localhost:10000/callback",
            None,
        )
        .expect("login code should exchange");

        restore_env("HOME", previous_home);
        restore_env("YAFFLE_MODULE_API_HOST", previous_host);

        assert_eq!(login.principal.principal_id, "principal-account-test");
        assert_eq!(login.principal.token, "account-token-test");
        assert!(!login.converted_from_anonymous);
    }

    fn restore_env(name: &str, value: Option<std::ffi::OsString>) {
        if let Some(value) = value {
            env::set_var(name, value);
        } else {
            env::remove_var(name);
        }
    }

    struct TestServer {
        authority: String,
        _join_handle: thread::JoinHandle<()>,
    }

    impl TestServer {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
            let authority = listener
                .local_addr()
                .expect("address should exist")
                .to_string();
            let join_handle = thread::spawn(move || {
                for _ in 0..4 {
                    let (mut stream, _) = listener.accept().expect("connection should accept");
                    let mut buffer = [0_u8; 8192];
                    let bytes_read = stream.read(&mut buffer).expect("request should read");
                    let request = String::from_utf8_lossy(&buffer[..bytes_read]);

                    if request.starts_with("POST /api/sessions/anonymous HTTP/1.1") {
                        assert!(!request.contains("feature-token:"));
                        let body = r#"{"data":{"principal_id":"principal-test","session_id":"session-test","token":"principal-token-test","issued_at":"2026-04-28T00:00:00Z","expires_at":"2030-04-28T00:00:00Z"}}"#;
                        write_response(&mut stream, body);
                    } else if request.starts_with("POST /api/execution-tokens HTTP/1.1") {
                        assert!(request.contains("authorization: Bearer principal-token-test"));
                        assert!(!request.contains("feature-token:"));
                        let body = if request.contains(r#""sessionKind":"shell_session""#) {
                            r#"{"data":{"token":"execution-token-shell-session","repo_binding_id":"binding-test","expires_at":"2030-04-28T04:00:00Z"}}"#
                        } else {
                            assert!(request.contains(r#""sessionKind":"workspace_init""#));
                            r#"{"data":{"token":"execution-token-test","repo_binding_id":"binding-test","expires_at":"2030-04-28T00:15:00Z"}}"#
                        };
                        write_response(&mut stream, body);
                    } else if request.starts_with("PUT /api/output-modules HTTP/1.1") {
                        assert!(request.contains("authorization: Bearer principal-token-test"));
                        assert!(!request.contains("feature-token:"));
                        let body = r#"{"data":{"id":"module-test","repo_binding_id":"binding-test","workspace_path":"infra/shared","environment_name":"pr-42","version_serial":1,"version":"1.0.1","created_at":"2030-04-28T00:00:00Z"}}"#;
                        write_response(&mut stream, body);
                    } else if request.starts_with("POST /api/cloud/cli/authorize-requests HTTP/1.1")
                    {
                        assert!(!request.contains("feature-token:"));
                        assert!(request.contains(r#""redirect_port":10000"#));
                        assert!(!request.contains("localhost"));
                        assert!(!request.contains("redirect_uri"));
                        assert!(request.contains(r#""code_challenge":"#));
                        let body = r#"{"data":{"authorizeUrl":"https://yaffle.dev/api/cloud/cli/authorize?request=opaque-test","expiresAt":"2030-04-28T00:05:00Z"}}"#;
                        write_response(&mut stream, body);
                    } else if request.starts_with("POST /api/cloud/cli/token HTTP/1.1") {
                        assert!(!request.contains("feature-token:"));
                        assert!(request.contains(r#""redirect_port":10000"#));
                        assert!(!request.contains("localhost"));
                        assert!(!request.contains("redirect_uri"));
                        assert!(request.contains(r#""code":"code-test""#));
                        let body = r#"{"data":{"principalId":"principal-account-test","principalType":"account","token":"account-token-test","issuedAt":"2026-04-28T00:00:00Z","expiresAt":"2030-04-28T00:00:00Z","userId":"user-test","userEmail":"test@example.com","userName":"Test User","convertedFromAnonymous":false}}"#;
                        write_response(&mut stream, body);
                    } else {
                        panic!("unexpected request: {request}");
                    }
                }
            });

            Self {
                authority,
                _join_handle: join_handle,
            }
        }

        fn authority(&self) -> &str {
            &self.authority
        }
    }

    fn write_response(stream: &mut std::net::TcpStream, body: &str) {
        let response = format!(
            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("response should write");
    }
}
