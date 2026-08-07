// Keep the v1 error contract and orchestration seams stable for the initial public release.
#![allow(clippy::result_large_err, clippy::too_many_arguments)]

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io;
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

mod local_first;

use hcl::eval::{Context as HclContext, Evaluate};
use hcl::Body;
use hmac::{Hmac, Mac};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use yaffle_contracts::{
    DiagnosticLevel, DiagnosticMessage, EngineOperation, EngineResponse, EnvironmentSnapshot,
    OperationResult, OperationResultKind, TerraformOutput, WorkspaceSnapshot,
};
use yaffle_graph::{
    apply_workspace_selection, resolve_workspace_graph, EnvironmentKind, GraphError,
    ResolvedWorkspaceGraph, WorkspaceGraphOptions,
};
use yaffle_tofu::{inspect_tofu_resolution, TofuResolutionRequest, TofuSourceKind};

use crate::local_first::{
    create_lifecycle_item, create_lifecycle_run, dispatch_lifecycle_via_control_plane,
    ensure_anonymous_principal, get_lifecycle_item, get_lifecycle_state, mint_execution_credential,
    publish_hosted_output_module, ExecutionCredential, ExecutionCredentialKind,
    ExecutionCredentialRequest, HostedOutputModulePublishRequest, LifecycleItemRequest,
    LifecycleRunRequest, LocalFirstError,
};

const CANONICAL_YAFFLE_MODULE_HOST: &str = "yaffle.dev";
const MODULE_API_HOST_OVERRIDE_ENV_VAR: &str = "YAFFLE_MODULE_API_HOST";

pub use crate::local_first::{
    build_cloud_cli_authorize_url, check_lifecycle_admission, clear_local_cloud_auth,
    compute_local_repo_fingerprint, exchange_cloud_cli_login_code, get_cloud_cli_capabilities,
    get_cloud_cli_inventory, get_cloud_remote_converge_status, load_local_cloud_auth_status,
    local_auth_store_path, module_api_base_url, start_cloud_remote_converge, CloudCliCapabilities,
    CloudCliInventory, CloudCliInventoryEnvironment, CloudCliInventoryStatusCount,
    CloudCliLoginResult, CloudRemoteConvergeHandle, CloudRemoteConvergeRequest,
    CloudRemoteConvergeStatus, CloudRemoteDeploymentStatus, CloudRemoteLatestRunSummary,
    LifecycleAdmissionRequest, LocalCloudAuthStatus, StoredPrincipalCredential,
    StoredPrincipalType,
};
use yaffle_config::{
    environment_name_matches_patterns, parse_yaffle_toml, validate_environment_name,
    LifecycleFailurePolicy, LifecycleGitHubRepositoryDispatch, LifecycleHook,
    LifecycleHookDispatch, LifecycleWebhookAuthScheme, YaffleConfig,
};
pub use yaffle_contracts::{EngineError, EnvironmentTarget, WorkspaceSelection, CONTRACT_VERSION};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvergeWorkspacePhase {
    PreparingAuth,
    InitializingTofu,
    ApplyingTofu,
    RecordingState,
    CollectingOutputs,
    PublishingOutputs,
    Completed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EngineProgressEvent {
    ConvergePlan {
        environment_name: String,
        workspaces: Vec<String>,
        dag: String,
    },
    WorkspacePhase {
        workspace_path: String,
        phase: ConvergeWorkspacePhase,
    },
    TofuLog {
        workspace_path: String,
        stream: TofuLogStream,
        line: String,
    },
    WorkspaceOutputs {
        workspace_path: String,
        outputs: BTreeMap<String, TerraformOutput>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TofuLogStream {
    Stdout,
    Stderr,
}

pub trait EngineProgressReporter {
    fn emit(&mut self, event: EngineProgressEvent);
}

impl<F> EngineProgressReporter for F
where
    F: FnMut(EngineProgressEvent),
{
    fn emit(&mut self, event: EngineProgressEvent) {
        self(event)
    }
}

struct ChannelProgressReporter {
    tx: mpsc::Sender<ConvergeWorkerMessage>,
}

impl EngineProgressReporter for ChannelProgressReporter {
    fn emit(&mut self, event: EngineProgressEvent) {
        let _ = self.tx.send(ConvergeWorkerMessage::Progress(event));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineRequest {
    pub operation: EngineOperation,
    pub target: Option<EnvironmentTarget>,
    pub selection: WorkspaceSelection,
    pub wait_for: Option<String>,
}

#[derive(Debug, Clone)]
struct RepoContext {
    repo_root: PathBuf,
    config_path: PathBuf,
    config: YaffleConfig,
    current_namespace: Option<String>,
}

#[derive(Debug, Clone)]
struct GraphContext {
    graph: ResolvedWorkspaceGraph,
    topological_order: Vec<String>,
}

#[derive(Debug, Clone)]
struct PreparedExecutionRepo {
    _temp_dir: Arc<tempfile::TempDir>,
    repo_root: PathBuf,
    tf_data_dir: PathBuf,
}

#[derive(Debug, Clone)]
struct PreparedWorkspaceExecution {
    uses_local_backend: bool,
    auth: PreparedWorkspaceAuth,
}

#[derive(Debug)]
struct WorkspaceConvergeOutcome {
    workspace_path: String,
    outputs: BTreeMap<String, TerraformOutput>,
    diagnostics: Vec<DiagnosticMessage>,
}

#[derive(Debug)]
enum ConvergeWorkerMessage {
    Progress(EngineProgressEvent),
    Finished(Result<WorkspaceConvergeOutcome, EngineError>),
}

#[derive(Debug, Clone, Default)]
struct PreparedWorkspaceAuth {
    tf_cli_config_file: Option<PathBuf>,
    required_hosts: Vec<String>,
    resolved_hosts: Vec<ResolvedAuthHost>,
    missing_hosts: Vec<String>,
}

#[derive(Debug, Clone)]
struct ResolvedAuthHost {
    host: String,
    source: AuthCredentialSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthCredentialSource {
    CliConfig,
    EnvToken,
    YaffleExecutionToken,
}

#[derive(Debug, Deserialize)]
struct RawTerraformOutput {
    value: Value,
    #[serde(rename = "type")]
    type_name: Option<Value>,
    sensitive: Option<bool>,
}

#[derive(Debug, Clone)]
struct WorkspaceStatusObservation {
    workspace_path: String,
    materialization: String,
    outputs: BTreeMap<String, TerraformOutput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitCondition {
    InfraReady,
    ActivationSettled,
    VerificationSettled,
    Usable,
    Acceptable,
    TeardownSettled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionMode {
    Any,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OperationPolicy {
    requires_target: bool,
    requires_repo_context: bool,
    selection_mode: SelectionMode,
}

pub fn execute(request: &EngineRequest, working_dir: &Path) -> Result<EngineResponse, EngineError> {
    execute_internal(request, working_dir, None)
}

pub fn execute_with_progress(
    request: &EngineRequest,
    working_dir: &Path,
    reporter: &mut dyn EngineProgressReporter,
) -> Result<EngineResponse, EngineError> {
    execute_internal(request, working_dir, Some(reporter))
}

fn execute_internal(
    request: &EngineRequest,
    working_dir: &Path,
    mut reporter: Option<&mut dyn EngineProgressReporter>,
) -> Result<EngineResponse, EngineError> {
    validate_request(request)?;

    let policy = operation_policy(&request.operation);
    let repo_context = if policy.requires_repo_context {
        Some(load_repo_context(working_dir, request)?)
    } else {
        None
    };

    match request.operation {
        EngineOperation::Graph => execute_graph_operation(
            request,
            repo_context
                .as_ref()
                .expect("graph execution should always have repo context"),
        ),
        EngineOperation::Converge => execute_converge_operation(
            request,
            repo_context
                .as_ref()
                .expect("converge execution should always have repo context"),
            &mut reporter,
        ),
        EngineOperation::Outputs => execute_outputs_operation(
            request,
            repo_context
                .as_ref()
                .expect("outputs execution should always have repo context"),
        ),
        EngineOperation::Status => execute_status_operation(
            request,
            repo_context
                .as_ref()
                .expect("status execution should always have repo context"),
        ),
        EngineOperation::Wait => execute_wait_operation(
            request,
            repo_context
                .as_ref()
                .expect("wait execution should always have repo context"),
        ),
        EngineOperation::Destroy => execute_destroy_operation(
            request,
            repo_context
                .as_ref()
                .expect("destroy execution should always have repo context"),
        ),
        EngineOperation::Doctor => Ok(execute_doctor_operation(request, working_dir)),
    }
}

pub fn prepare_tf_login_exports(
    working_dir: &Path,
    environment_name: &str,
    workspace_path: &str,
) -> Result<String, EngineError> {
    let request = EngineRequest {
        operation: EngineOperation::Outputs,
        target: Some(EnvironmentTarget {
            environment: environment_name.to_string(),
        }),
        selection: WorkspaceSelection {
            workspaces: vec![workspace_path.to_string()],
        },
        wait_for: None,
    };

    validate_request(&request)?;
    let repo_context = load_repo_context(working_dir, &request)?;
    let graph_context = load_graph_context(&repo_context, &request)?;
    let canonical_repo_namespace = repo_context.current_namespace.as_ref().ok_or_else(|| {
        request_error(
            &request,
            "repo_namespace_unresolved",
            "Could not infer repo namespace for hosted Yaffle module auth. Configure a canonical git remote before using yaffle tf login.",
        )
    })?;
    let local_repo_fingerprint = compute_local_repo_fingerprint(&repo_context.repo_root)
        .map_err(|error| local_first_error(&request, "repo_fingerprint_failed", error))?;
    let principal = ensure_anonymous_principal().map_err(|error| {
        local_first_error(&request, "anonymous_session_bootstrap_failed", error)
    })?;
    let execution_credential = mint_execution_credential(
        &principal,
        &ExecutionCredentialRequest {
            canonical_repo_namespace,
            local_repo_fingerprint: &local_repo_fingerprint,
            environment_name,
            consumer_workspace_path: workspace_path,
            session_kind: ExecutionCredentialKind::ShellSession,
        },
    )
    .map_err(|error| local_first_error(&request, "execution_token_mint_failed", error))?;

    let credentials_path = write_tf_login_credentials_file(
        &request,
        environment_name,
        workspace_path,
        &effective_yaffle_module_host(),
        &execution_credential,
    )?;

    let mut exports = vec![format!(
        "export TF_CLI_CONFIG_FILE={}",
        shell_single_quote(&credentials_path.display().to_string())
    )];

    let workspace_dir = repo_context.repo_root.join(workspace_path);
    let has_explicit_backend =
        workspace_has_explicit_backend(&request, &workspace_dir).map_err(|error| {
            request_error(
                &request,
                "workspace_backend_detect_failed",
                format!(
                    "Failed to inspect backend configuration for workspace '{}': {}",
                    workspace_path, error.error.message
                ),
            )
        })?;

    if !has_explicit_backend {
        let state_path =
            local_backend_state_path(&repo_context.repo_root, environment_name, workspace_path);
        let state_dir = state_path
            .parent()
            .expect("local backend state path should have a parent");
        fs::create_dir_all(state_dir).map_err(|error| {
            request_error(
                &request,
                "auth_config_write_failed",
                format!(
                    "Failed to create local state directory for workspace '{}': {error}",
                    workspace_path
                ),
            )
        })?;

        let tf_data_dir =
            local_tf_data_dir(&repo_context.repo_root, environment_name, workspace_path);
        fs::create_dir_all(&tf_data_dir).map_err(|error| {
            request_error(
                &request,
                "auth_config_write_failed",
                format!(
                    "Failed to create OpenTofu data directory for workspace '{}': {error}",
                    workspace_path
                ),
            )
        })?;

        exports.push(format!(
            "export TF_DATA_DIR={}",
            shell_single_quote(&tf_data_dir.display().to_string())
        ));

        let state_arg = format!("-state={}", state_path.display());
        for variable_name in [
            "TF_CLI_ARGS_plan",
            "TF_CLI_ARGS_apply",
            "TF_CLI_ARGS_destroy",
            "TF_CLI_ARGS_output",
            "TF_CLI_ARGS_refresh",
            "TF_CLI_ARGS_import",
        ] {
            exports.push(shell_append_export(variable_name, &state_arg));
        }
    }

    if let Some(module_host_override) = module_api_host_override() {
        exports.push(format!(
            "export TF_VAR_module_registry_host={}",
            shell_single_quote(&module_host_override)
        ));
    }

    exports.push(format!(
        "export TF_VAR_environment={}",
        shell_single_quote(environment_name)
    ));
    exports.push(format!(
        "export TF_VAR_environment_kind={}",
        shell_single_quote(environment_kind_name(
            graph_context
                .graph
                .environment_kind
                .unwrap_or(EnvironmentKind::Named)
        ))
    ));
    exports.push(format!(
        "export YAFFLE_ACTIVE_ENV={}",
        shell_single_quote(environment_name)
    ));
    exports.push(format!(
        "export YAFFLE_ACTIVE_WORKSPACE={}",
        shell_single_quote(workspace_path)
    ));

    Ok(exports.join("\n") + "\n")
}

fn execute_graph_operation(
    request: &EngineRequest,
    repo_context: &RepoContext,
) -> Result<EngineResponse, EngineError> {
    let graph_context = load_graph_context(repo_context, request)?;
    let mut diagnostics = repo_context_diagnostics(repo_context);
    let dependents = build_dependents(&graph_context.graph);

    let workspace_snapshots = graph_context
        .topological_order
        .iter()
        .map(|workspace_path| WorkspaceSnapshot {
            workspace_path: workspace_path.clone(),
            lifecycle: None,
            materialization: None,
            freshness: None,
        })
        .collect();

    for (index, workspace_path) in graph_context.topological_order.iter().enumerate() {
        let node = graph_context
            .graph
            .workspace(workspace_path)
            .expect("ordered workspace should exist in resolved graph");

        diagnostics.push(DiagnosticMessage {
            level: DiagnosticLevel::Info,
            code: Some("graph_node".to_string()),
            message: format!("Resolved graph node '{}'.", workspace_path),
            workspace_path: Some(workspace_path.clone()),
            item_key: None,
            details: Some(BTreeMap::from([
                ("dependencies".to_string(), json!(node.dependencies)),
                (
                    "dependents".to_string(),
                    json!(dependents.get(workspace_path).cloned().unwrap_or_default()),
                ),
                ("topological_index".to_string(), json!(index)),
            ])),
        });
    }

    Ok(build_response(
        request,
        OperationResultKind::Succeeded,
        format_graph_summary(&graph_context.graph, &graph_context.topological_order),
        workspace_snapshots,
        BTreeMap::new(),
        diagnostics,
    ))
}

fn execute_outputs_operation(
    request: &EngineRequest,
    repo_context: &RepoContext,
) -> Result<EngineResponse, EngineError> {
    let graph_context = load_graph_context(repo_context, request)?;

    let tofu_report = inspect_tofu_resolution(&TofuResolutionRequest::default());
    let tofu_resolution = tofu_report.clone().into_result().map_err(|error| {
        request_error_with_details(
            request,
            "tofu_resolution_failed",
            "could not resolve tofu using the configured source policy",
            Some(BTreeMap::from([(
                "attempts".to_string(),
                json!(error.attempts),
            )])),
        )
    })?;

    let prepared_repo = prepare_execution_repo(&repo_context.repo_root, ".", request)?;
    let environment_kind = graph_context
        .graph
        .environment_kind
        .expect("outputs operation should have an environment kind");
    let mut diagnostics = repo_context_diagnostics(repo_context);
    let mut workspace_outputs = BTreeMap::new();

    for workspace_path in &graph_context.topological_order {
        let workspace_config = repo_context
            .config
            .workspaces
            .iter()
            .find(|workspace| workspace.path == *workspace_path)
            .expect("selected outputs workspace should exist in config");

        let workspace_execution = configure_workspace_execution(
            request,
            repo_context,
            &prepared_repo,
            workspace_config,
            environment_kind,
        )?;

        append_auth_diagnostics(&mut diagnostics, workspace_path, &workspace_execution.auth);

        run_tofu_command(
            request,
            &tofu_resolution,
            &prepared_repo,
            &workspace_execution,
            workspace_path,
            &["init", "-input=false", "-no-color"],
            "tofu_init_failed",
        )?;

        let output = run_tofu_command(
            request,
            &tofu_resolution,
            &prepared_repo,
            &workspace_execution,
            workspace_path,
            &["output", "-json", "-no-color"],
            "tofu_output_failed",
        )?;
        let outputs = parse_terraform_outputs(request, workspace_path, &output.stdout)?;
        let safe_outputs = redact_sensitive_outputs(request, &outputs)?;
        workspace_outputs.insert(workspace_path.clone(), safe_outputs);

        diagnostics.push(DiagnosticMessage {
            level: DiagnosticLevel::Info,
            code: Some("tofu_resolved".to_string()),
            message: format!(
                "Resolved tofu via {} at '{}' ({})",
                tofu_source_label(tofu_resolution.source),
                tofu_resolution.path.display(),
                tofu_resolution.version
            ),
            workspace_path: Some(workspace_path.clone()),
            item_key: None,
            details: Some(BTreeMap::from([
                ("source".to_string(), json!(tofu_resolution.source)),
                (
                    "path".to_string(),
                    json!(tofu_resolution.path.display().to_string()),
                ),
                ("version".to_string(), json!(tofu_resolution.version)),
                ("attempts".to_string(), json!(tofu_report.attempts)),
            ])),
        });
        diagnostics.push(DiagnosticMessage {
            level: DiagnosticLevel::Info,
            code: Some("outputs_loaded".to_string()),
            message: format!(
                "Loaded {} output(s) for workspace '{}'.",
                outputs.len(),
                workspace_path
            ),
            workspace_path: Some(workspace_path.clone()),
            item_key: None,
            details: Some(BTreeMap::from([(
                "output_keys".to_string(),
                json!(outputs.keys().cloned().collect::<Vec<_>>()),
            )])),
        });
    }

    let workspace_snapshots = graph_context
        .graph
        .workspaces
        .iter()
        .map(|workspace| WorkspaceSnapshot {
            workspace_path: workspace.path.clone(),
            lifecycle: None,
            materialization: None,
            freshness: None,
        })
        .collect::<Vec<_>>();

    let environment_name = request
        .target
        .as_ref()
        .map(|target| target.environment.as_str())
        .unwrap_or("unknown");

    let flat_outputs = if workspace_outputs.len() == 1 {
        workspace_outputs
            .values()
            .next()
            .cloned()
            .unwrap_or_default()
    } else {
        BTreeMap::new()
    };

    let mut response = build_response(
        request,
        OperationResultKind::Succeeded,
        format_outputs_summary(
            environment_name,
            &graph_context.topological_order,
            &workspace_outputs,
        ),
        workspace_snapshots,
        flat_outputs,
        diagnostics,
    );
    response.workspace_outputs = workspace_outputs;

    Ok(response)
}

fn execute_status_operation(
    request: &EngineRequest,
    repo_context: &RepoContext,
) -> Result<EngineResponse, EngineError> {
    let graph_context = load_graph_context(repo_context, request)?;
    let tofu_report = inspect_tofu_resolution(&TofuResolutionRequest::default());
    let tofu_resolution = tofu_report.clone().into_result().map_err(|error| {
        request_error_with_details(
            request,
            "tofu_resolution_failed",
            "could not resolve tofu using the configured source policy",
            Some(BTreeMap::from([(
                "attempts".to_string(),
                json!(error.attempts),
            )])),
        )
    })?;
    let prepared_repo = prepare_execution_repo(&repo_context.repo_root, ".", request)?;
    let environment_kind = graph_context
        .graph
        .environment_kind
        .expect("status operation should have an environment kind");

    let mut diagnostics = repo_context_diagnostics(repo_context);
    diagnostics.push(DiagnosticMessage {
        level: DiagnosticLevel::Info,
        code: Some("tofu_resolved".to_string()),
        message: format!(
            "Resolved tofu via {} at '{}' ({})",
            tofu_source_label(tofu_resolution.source),
            tofu_resolution.path.display(),
            tofu_resolution.version
        ),
        workspace_path: None,
        item_key: None,
        details: Some(BTreeMap::from([
            ("source".to_string(), json!(tofu_resolution.source)),
            (
                "path".to_string(),
                json!(tofu_resolution.path.display().to_string()),
            ),
            ("version".to_string(), json!(tofu_resolution.version)),
            ("attempts".to_string(), json!(tofu_report.attempts)),
        ])),
    });

    let mut observations = Vec::new();
    let mut workspace_error_count = 0usize;
    for workspace_path in &graph_context.topological_order {
        let observation_result: Result<WorkspaceStatusObservation, EngineError> = (|| {
            let workspace_config = repo_context
                .config
                .workspaces
                .iter()
                .find(|workspace| workspace.path == *workspace_path)
                .expect("status workspace should exist in config");
            let workspace_execution = configure_workspace_execution(
                request,
                repo_context,
                &prepared_repo,
                workspace_config,
                environment_kind,
            )?;

            append_auth_diagnostics(&mut diagnostics, workspace_path, &workspace_execution.auth);

            run_tofu_command(
                request,
                &tofu_resolution,
                &prepared_repo,
                &workspace_execution,
                workspace_path,
                &["init", "-input=false", "-no-color"],
                "tofu_init_failed",
            )?;

            inspect_workspace_status(
                request,
                &prepared_repo,
                &tofu_resolution,
                &workspace_execution,
                workspace_path,
            )
        })();

        match observation_result {
            Ok(observation) => {
                diagnostics.push(DiagnosticMessage {
                    level: DiagnosticLevel::Info,
                    code: Some("workspace_status".to_string()),
                    message: format!(
                        "Workspace '{}' is {} with {} output(s).",
                        observation.workspace_path,
                        observation.materialization,
                        observation.outputs.len()
                    ),
                    workspace_path: Some(observation.workspace_path.clone()),
                    item_key: None,
                    details: Some(BTreeMap::from([
                        (
                            "materialization".to_string(),
                            json!(observation.materialization.clone()),
                        ),
                        ("output_count".to_string(), json!(observation.outputs.len())),
                        (
                            "output_keys".to_string(),
                            json!(observation.outputs.keys().cloned().collect::<Vec<_>>()),
                        ),
                    ])),
                });
                observations.push(observation);
            }
            Err(error) => {
                workspace_error_count += 1;
                diagnostics.push(DiagnosticMessage {
                    level: DiagnosticLevel::Error,
                    code: Some(error.error.code),
                    message: error.error.message,
                    workspace_path: Some(workspace_path.clone()),
                    item_key: None,
                    details: error.error.details,
                });
                observations.push(WorkspaceStatusObservation {
                    workspace_path: workspace_path.clone(),
                    materialization: "partially_present".to_string(),
                    outputs: BTreeMap::new(),
                });
            }
        }
    }

    let environment_name = request
        .target
        .as_ref()
        .map(|target| target.environment.as_str())
        .unwrap_or("unknown");
    let environment_materialization = derive_environment_materialization(&observations);
    let lifecycle_state =
        try_load_lifecycle_state(repo_context, environment_name, &mut diagnostics);
    let workspace_snapshots = observations
        .iter()
        .map(|observation| WorkspaceSnapshot {
            workspace_path: observation.workspace_path.clone(),
            lifecycle: build_workspace_lifecycle_json(
                &observation.workspace_path,
                lifecycle_state.as_ref(),
            ),
            materialization: Some(observation.materialization.clone()),
            freshness: None,
        })
        .collect::<Vec<_>>();
    let result_kind = if workspace_error_count > 0 {
        OperationResultKind::Degraded
    } else {
        OperationResultKind::Succeeded
    };

    Ok(build_response_with_environment(
        request,
        result_kind.clone(),
        format_status_summary(
            environment_name,
            &observations,
            &environment_materialization,
        ),
        Some(EnvironmentSnapshot {
            lifecycle: build_environment_lifecycle_json(lifecycle_state.as_ref()),
            conditions: build_environment_conditions(
                result_kind,
                Some(environment_materialization.as_str()),
                lifecycle_state.as_ref(),
            ),
            materialization: Some(environment_materialization),
            freshness: None,
        }),
        workspace_snapshots,
        BTreeMap::new(),
        diagnostics,
    ))
}

fn execute_wait_operation(
    request: &EngineRequest,
    repo_context: &RepoContext,
) -> Result<EngineResponse, EngineError> {
    let condition_name = request
        .wait_for
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    let condition = parse_wait_condition(condition_name)
        .ok_or_else(|| request_error(request, "invalid_condition", format!("unsupported wait condition '{}': expected one of infra_ready, activation_settled, verification_settled, usable, acceptable, teardown_settled", condition_name)))?;
    let timeout = wait_timeout();
    let poll_interval = wait_poll_interval();
    let deadline = Instant::now() + timeout;
    let status_request = EngineRequest {
        operation: EngineOperation::Status,
        target: request.target.clone(),
        selection: WorkspaceSelection::default(),
        wait_for: None,
    };
    let mut attempts = 0usize;

    loop {
        attempts += 1;
        let status_response = execute_status_operation(&status_request, repo_context)?;
        let met = wait_condition_met(condition, &status_response);

        if met {
            let mut diagnostics = status_response.diagnostics;
            diagnostics.push(DiagnosticMessage {
                level: DiagnosticLevel::Info,
                code: Some("wait_condition_met".to_string()),
                message: format!(
                    "Condition '{}' was met after {} attempt(s).",
                    condition_name, attempts
                ),
                workspace_path: None,
                item_key: None,
                details: Some(BTreeMap::from([
                    ("condition".to_string(), json!(condition_name)),
                    ("attempts".to_string(), json!(attempts)),
                ])),
            });

            return Ok(build_response_with_environment(
                request,
                OperationResultKind::Succeeded,
                format!(
                    "condition '{}' met for environment '{}' after {} attempt(s)",
                    condition_name,
                    request
                        .target
                        .as_ref()
                        .map(|target| target.environment.as_str())
                        .unwrap_or("unknown"),
                    attempts
                ),
                status_response.environment,
                status_response.workspaces,
                status_response.outputs,
                diagnostics,
            ));
        }

        if status_response.result.kind == OperationResultKind::Degraded {
            let mut diagnostics = status_response.diagnostics;
            diagnostics.push(DiagnosticMessage {
                level: DiagnosticLevel::Warning,
                code: Some("wait_condition_blocked".to_string()),
                message: format!(
                    "Condition '{}' could not be evaluated cleanly because status is degraded.",
                    condition_name
                ),
                workspace_path: None,
                item_key: None,
                details: Some(BTreeMap::from([(
                    "condition".to_string(),
                    json!(condition_name),
                )])),
            });

            return Ok(build_response_with_environment(
                request,
                OperationResultKind::Blocked,
                format!(
                    "condition '{}' blocked for environment '{}' because status is degraded",
                    condition_name,
                    request
                        .target
                        .as_ref()
                        .map(|target| target.environment.as_str())
                        .unwrap_or("unknown")
                ),
                status_response.environment,
                status_response.workspaces,
                status_response.outputs,
                diagnostics,
            ));
        }

        if Instant::now() >= deadline {
            let mut diagnostics = status_response.diagnostics;
            diagnostics.push(DiagnosticMessage {
                level: DiagnosticLevel::Warning,
                code: Some("wait_condition_timeout".to_string()),
                message: format!(
                    "Timed out waiting for condition '{}' after {} attempt(s).",
                    condition_name, attempts
                ),
                workspace_path: None,
                item_key: None,
                details: Some(BTreeMap::from([
                    ("condition".to_string(), json!(condition_name)),
                    ("attempts".to_string(), json!(attempts)),
                    ("timeout_ms".to_string(), json!(timeout.as_millis() as u64)),
                ])),
            });

            return Ok(build_response_with_environment(
                request,
                OperationResultKind::Blocked,
                format!(
                    "condition '{}' not met for environment '{}' within {}ms",
                    condition_name,
                    request
                        .target
                        .as_ref()
                        .map(|target| target.environment.as_str())
                        .unwrap_or("unknown"),
                    timeout.as_millis()
                ),
                status_response.environment,
                status_response.workspaces,
                status_response.outputs,
                diagnostics,
            ));
        }

        thread::sleep(poll_interval);
    }
}

fn execute_destroy_operation(
    request: &EngineRequest,
    repo_context: &RepoContext,
) -> Result<EngineResponse, EngineError> {
    let graph_context = load_graph_context(repo_context, request)?;
    let tofu_report = inspect_tofu_resolution(&TofuResolutionRequest::default());
    let tofu_resolution = tofu_report.clone().into_result().map_err(|error| {
        request_error_with_details(
            request,
            "tofu_resolution_failed",
            "could not resolve tofu using the configured source policy",
            Some(BTreeMap::from([(
                "attempts".to_string(),
                json!(error.attempts),
            )])),
        )
    })?;

    let prepared_repo = prepare_execution_repo(&repo_context.repo_root, ".", request)?;
    let environment_kind = graph_context
        .graph
        .environment_kind
        .expect("destroy operation should have an environment kind");
    let mut destroy_order = graph_context.topological_order.clone();
    destroy_order.reverse();
    let mut diagnostics = repo_context_diagnostics(repo_context);

    for workspace_path in &destroy_order {
        let workspace_config = repo_context
            .config
            .workspaces
            .iter()
            .find(|workspace| workspace.path == *workspace_path)
            .expect("destroy workspace should exist in config");
        let workspace_execution = configure_workspace_execution(
            request,
            repo_context,
            &prepared_repo,
            workspace_config,
            environment_kind,
        )?;

        append_auth_diagnostics(&mut diagnostics, workspace_path, &workspace_execution.auth);

        run_tofu_command(
            request,
            &tofu_resolution,
            &prepared_repo,
            &workspace_execution,
            workspace_path,
            &["init", "-input=false", "-no-color"],
            "tofu_init_failed",
        )?;
        let destroy_result = run_tofu_command(
            request,
            &tofu_resolution,
            &prepared_repo,
            &workspace_execution,
            workspace_path,
            &["destroy", "-auto-approve", "-input=false", "-no-color"],
            "tofu_destroy_failed",
        );

        if workspace_execution.uses_local_backend {
            settle_command_with_state_persistence(
                destroy_result,
                persist_local_backend_state(request, repo_context, &prepared_repo, workspace_path),
            )?;
            remove_local_backend_state(request, repo_context, workspace_path)?;
        } else {
            destroy_result?;
        }
    }

    diagnostics.push(DiagnosticMessage {
        level: DiagnosticLevel::Info,
        code: Some("tofu_resolved".to_string()),
        message: format!(
            "Resolved tofu via {} at '{}' ({})",
            tofu_source_label(tofu_resolution.source),
            tofu_resolution.path.display(),
            tofu_resolution.version
        ),
        workspace_path: None,
        item_key: None,
        details: Some(BTreeMap::from([
            ("source".to_string(), json!(tofu_resolution.source)),
            (
                "path".to_string(),
                json!(tofu_resolution.path.display().to_string()),
            ),
            ("version".to_string(), json!(tofu_resolution.version)),
            ("attempts".to_string(), json!(tofu_report.attempts)),
        ])),
    });

    for (index, workspace_path) in destroy_order.iter().enumerate() {
        diagnostics.push(DiagnosticMessage {
            level: DiagnosticLevel::Info,
            code: Some("workspace_destroyed".to_string()),
            message: format!("Destroyed workspace '{}'.", workspace_path),
            workspace_path: Some(workspace_path.clone()),
            item_key: None,
            details: Some(BTreeMap::from([(
                "reverse_topological_index".to_string(),
                json!(index),
            )])),
        });
    }

    let workspace_snapshots = destroy_order
        .iter()
        .map(|workspace_path| WorkspaceSnapshot {
            workspace_path: workspace_path.clone(),
            lifecycle: None,
            materialization: Some("absent".to_string()),
            freshness: None,
        })
        .collect::<Vec<_>>();

    Ok(build_response(
        request,
        OperationResultKind::Succeeded,
        format_destroy_summary(
            request
                .target
                .as_ref()
                .map(|target| target.environment.as_str())
                .unwrap_or("unknown"),
            &destroy_order,
        ),
        workspace_snapshots,
        BTreeMap::new(),
        diagnostics,
    ))
}

fn execute_converge_operation(
    request: &EngineRequest,
    repo_context: &RepoContext,
    reporter: &mut Option<&mut dyn EngineProgressReporter>,
) -> Result<EngineResponse, EngineError> {
    let graph_context = load_graph_context(repo_context, request)?;
    let tofu_report = inspect_tofu_resolution(&TofuResolutionRequest::default());
    let tofu_resolution = tofu_report.clone().into_result().map_err(|error| {
        request_error_with_details(
            request,
            "tofu_resolution_failed",
            "could not resolve tofu using the configured source policy",
            Some(BTreeMap::from([(
                "attempts".to_string(),
                json!(error.attempts),
            )])),
        )
    })?;

    let prepared_repo = prepare_execution_repo(&repo_context.repo_root, ".", request)?;
    let environment_kind = graph_context
        .graph
        .environment_kind
        .expect("converge operation should have an environment kind");
    let mut diagnostics = repo_context_diagnostics(repo_context);
    let environment_name = request
        .target
        .as_ref()
        .map(|target| target.environment.as_str())
        .unwrap_or("unknown");
    if selected_workspaces_have_lifecycle_hooks(
        repo_context,
        environment_name,
        &graph_context.topological_order,
    ) {
        preflight_environment_governance(request, repo_context, environment_name)?;
    }
    let mut lifecycle_run_context = None;
    let mut lifecycle_results = Vec::new();

    emit_progress(
        reporter,
        EngineProgressEvent::ConvergePlan {
            environment_name: request
                .target
                .as_ref()
                .map(|target| target.environment.clone())
                .unwrap_or_default(),
            workspaces: graph_context.topological_order.clone(),
            dag: render_graph_dag(&graph_context.graph, &graph_context.topological_order),
        },
    );

    let workspace_configs = repo_context
        .config
        .workspaces
        .iter()
        .cloned()
        .map(|workspace| (workspace.path.clone(), workspace))
        .collect::<BTreeMap<_, _>>();

    for level in workspace_execution_levels(request, repo_context, &graph_context)? {
        let (tx, rx) = mpsc::channel::<ConvergeWorkerMessage>();
        let mut remaining = level.len();

        for workspace_path in &level {
            let tx = tx.clone();
            let request = request.clone();
            let repo_context = repo_context.clone();
            let prepared_repo = prepared_repo.clone();
            let tofu_resolution = tofu_resolution.clone();
            let workspace = workspace_configs
                .get(workspace_path)
                .cloned()
                .expect("converge workspace should exist in config");

            thread::spawn(move || {
                let progress_tx = tx.clone();
                let result = execute_single_workspace_converge(
                    &request,
                    &repo_context,
                    &prepared_repo,
                    &tofu_resolution,
                    workspace,
                    environment_kind,
                    Some(progress_tx),
                );
                let _ = tx.send(ConvergeWorkerMessage::Finished(result));
            });
        }
        drop(tx);

        let mut level_outcomes = Vec::new();
        let mut level_errors = Vec::new();
        while remaining > 0 {
            match rx.recv() {
                Ok(ConvergeWorkerMessage::Progress(event)) => emit_progress(reporter, event),
                Ok(ConvergeWorkerMessage::Finished(result)) => {
                    remaining -= 1;
                    match result {
                        Ok(outcome) => level_outcomes.push(outcome),
                        Err(error) => level_errors.push(error),
                    }
                }
                Err(_) => {
                    return Err(request_error(
                        request,
                        "converge_worker_disconnected",
                        "Workspace converge worker disconnected unexpectedly.",
                    ))
                }
            }
        }

        let outcome_by_path = level_outcomes
            .into_iter()
            .map(|outcome| (outcome.workspace_path.clone(), outcome))
            .collect::<BTreeMap<_, _>>();

        for workspace_path in &level {
            let Some(outcome) = outcome_by_path.get(workspace_path) else {
                continue;
            };
            diagnostics.extend(outcome.diagnostics.clone());

            let workspace_config = workspace_configs
                .get(workspace_path)
                .expect("converge workspace should exist in config");
            lifecycle_results.extend(execute_lifecycle_hooks_for_workspace(
                request,
                repo_context,
                workspace_config,
                workspace_path,
                &outcome.outputs,
                &mut lifecycle_run_context,
                &mut diagnostics,
                "activation",
            )?);
            lifecycle_results.extend(execute_lifecycle_hooks_for_workspace(
                request,
                repo_context,
                workspace_config,
                workspace_path,
                &outcome.outputs,
                &mut lifecycle_run_context,
                &mut diagnostics,
                "verification",
            )?);

            emit_progress(
                reporter,
                EngineProgressEvent::WorkspacePhase {
                    workspace_path: workspace_path.to_string(),
                    phase: ConvergeWorkspacePhase::Completed,
                },
            );
        }

        if let Some(error) = level_errors.into_iter().next() {
            return Err(error);
        }
    }

    diagnostics.push(DiagnosticMessage {
        level: DiagnosticLevel::Info,
        code: Some("tofu_resolved".to_string()),
        message: format!(
            "Resolved tofu via {} at '{}' ({})",
            tofu_source_label(tofu_resolution.source),
            tofu_resolution.path.display(),
            tofu_resolution.version
        ),
        workspace_path: None,
        item_key: None,
        details: Some(BTreeMap::from([
            ("source".to_string(), json!(tofu_resolution.source)),
            (
                "path".to_string(),
                json!(tofu_resolution.path.display().to_string()),
            ),
            ("version".to_string(), json!(tofu_resolution.version)),
            ("attempts".to_string(), json!(tofu_report.attempts)),
        ])),
    });

    for (index, workspace_path) in graph_context.topological_order.iter().enumerate() {
        diagnostics.push(DiagnosticMessage {
            level: DiagnosticLevel::Info,
            code: Some("workspace_converged".to_string()),
            message: format!("Converged workspace '{}'.", workspace_path),
            workspace_path: Some(workspace_path.clone()),
            item_key: None,
            details: Some(BTreeMap::from([(
                "topological_index".to_string(),
                json!(index),
            )])),
        });
    }

    let workspace_snapshots = graph_context
        .topological_order
        .iter()
        .map(|workspace_path| WorkspaceSnapshot {
            workspace_path: workspace_path.clone(),
            lifecycle: None,
            materialization: None,
            freshness: None,
        })
        .collect::<Vec<_>>();

    let result_kind =
        lifecycle_results
            .iter()
            .fold(OperationResultKind::Succeeded, |kind, item| {
                match (kind.clone(), item.state.as_str(), item.failure_policy) {
                    (_, "failed", LifecycleFailurePolicy::Failed) => OperationResultKind::Failed,
                    (OperationResultKind::Succeeded, "degraded", _) => {
                        OperationResultKind::Degraded
                    }
                    (
                        OperationResultKind::Succeeded,
                        "failed",
                        LifecycleFailurePolicy::Degraded,
                    ) => OperationResultKind::Degraded,
                    _ => kind,
                }
            });
    let summary = if lifecycle_results.is_empty() {
        format_converge_summary(environment_name, &graph_context.topological_order)
    } else {
        format!(
            "{}\n\nlifecycle settled: {} item(s)",
            format_converge_summary(environment_name, &graph_context.topological_order),
            lifecycle_results.len()
        )
    };

    Ok(build_response(
        request,
        result_kind,
        summary,
        workspace_snapshots,
        BTreeMap::new(),
        diagnostics,
    ))
}

fn execute_single_workspace_converge(
    request: &EngineRequest,
    repo_context: &RepoContext,
    prepared_repo: &PreparedExecutionRepo,
    tofu_resolution: &yaffle_tofu::TofuResolution,
    workspace: yaffle_config::Workspace,
    environment_kind: EnvironmentKind,
    progress_tx: Option<mpsc::Sender<ConvergeWorkerMessage>>,
) -> Result<WorkspaceConvergeOutcome, EngineError> {
    let workspace_path = workspace.path.clone();
    emit_progress_via_channel(
        &progress_tx,
        EngineProgressEvent::WorkspacePhase {
            workspace_path: workspace_path.clone(),
            phase: ConvergeWorkspacePhase::PreparingAuth,
        },
    );

    let workspace_execution = configure_workspace_execution(
        request,
        repo_context,
        prepared_repo,
        &workspace,
        environment_kind,
    )?;

    let mut diagnostics = Vec::new();
    append_auth_diagnostics(&mut diagnostics, &workspace_path, &workspace_execution.auth);

    emit_progress_via_channel(
        &progress_tx,
        EngineProgressEvent::WorkspacePhase {
            workspace_path: workspace_path.clone(),
            phase: ConvergeWorkspacePhase::InitializingTofu,
        },
    );
    run_tofu_command(
        request,
        tofu_resolution,
        prepared_repo,
        &workspace_execution,
        &workspace_path,
        &["init", "-input=false", "-no-color"],
        "tofu_init_failed",
    )?;

    emit_progress_via_channel(
        &progress_tx,
        EngineProgressEvent::WorkspacePhase {
            workspace_path: workspace_path.clone(),
            phase: ConvergeWorkspacePhase::ApplyingTofu,
        },
    );
    let mut channel_reporter = progress_tx.clone().map(|tx| ChannelProgressReporter { tx });
    let mut progress_reporter = channel_reporter
        .as_mut()
        .map(|reporter| reporter as &mut dyn EngineProgressReporter);
    let apply_result = run_tofu_command_with_progress(
        request,
        tofu_resolution,
        prepared_repo,
        &workspace_execution,
        &workspace_path,
        &["apply", "-auto-approve", "-input=false", "-no-color"],
        "tofu_apply_failed",
        &mut progress_reporter,
    );

    if workspace_execution.uses_local_backend {
        emit_progress_via_channel(
            &progress_tx,
            EngineProgressEvent::WorkspacePhase {
                workspace_path: workspace_path.clone(),
                phase: ConvergeWorkspacePhase::RecordingState,
            },
        );
        settle_command_with_state_persistence(
            apply_result,
            persist_local_backend_state(request, repo_context, prepared_repo, &workspace_path),
        )?;
    } else {
        apply_result?;
    }

    emit_progress_via_channel(
        &progress_tx,
        EngineProgressEvent::WorkspacePhase {
            workspace_path: workspace_path.clone(),
            phase: ConvergeWorkspacePhase::CollectingOutputs,
        },
    );
    let output = run_tofu_command(
        request,
        tofu_resolution,
        prepared_repo,
        &workspace_execution,
        &workspace_path,
        &["output", "-json", "-no-color"],
        "tofu_output_failed",
    )?;
    let outputs = parse_terraform_outputs(request, &workspace_path, &output.stdout)?;
    let safe_outputs = redact_sensitive_outputs(request, &outputs)?;
    let selected_outputs = select_workspace_outputs(request, &workspace, &outputs)?;
    emit_progress_via_channel(
        &progress_tx,
        EngineProgressEvent::WorkspaceOutputs {
            workspace_path: workspace_path.clone(),
            outputs: safe_outputs.clone(),
        },
    );

    emit_progress_via_channel(
        &progress_tx,
        EngineProgressEvent::WorkspacePhase {
            workspace_path: workspace_path.clone(),
            phase: ConvergeWorkspacePhase::PublishingOutputs,
        },
    );
    if let Some(published_version) = maybe_publish_hosted_output_module(
        request,
        repo_context,
        &workspace_path,
        &workspace.outputs.keys().cloned().collect::<Vec<_>>(),
        &selected_outputs,
    )? {
        diagnostics.push(DiagnosticMessage {
            level: DiagnosticLevel::Info,
            code: Some("hosted_output_module_published".to_string()),
            message: format!(
                "Published hosted output module '{}' for environment '{}' as version '{}'.",
                workspace_path,
                request
                    .target
                    .as_ref()
                    .map(|target| target.environment.as_str())
                    .unwrap_or("unknown"),
                published_version
            ),
            workspace_path: Some(workspace_path.clone()),
            item_key: None,
            details: Some(BTreeMap::from([(
                "version".to_string(),
                json!(published_version),
            )])),
        });
    }

    Ok(WorkspaceConvergeOutcome {
        workspace_path,
        outputs: safe_outputs,
        diagnostics,
    })
}

fn workspace_execution_levels(
    request: &EngineRequest,
    repo_context: &RepoContext,
    graph_context: &GraphContext,
) -> Result<Vec<Vec<String>>, EngineError> {
    let environment_name = request
        .target
        .as_ref()
        .map(|target| target.environment.as_str())
        .unwrap_or("unknown");
    let implicit_dependencies = graph_context
        .topological_order
        .iter()
        .map(|workspace_path| {
            implicit_local_remote_state_dependencies(
                request,
                &repo_context.repo_root,
                workspace_path,
                environment_name,
            )
            .map(|dependencies| (workspace_path.clone(), dependencies))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;

    let mut stages = BTreeMap::<String, usize>::new();
    for workspace_path in &graph_context.topological_order {
        let dependencies = graph_context
            .graph
            .workspace(workspace_path)
            .map(|workspace| workspace.dependencies.clone())
            .unwrap_or_default();
        let stage = dependencies
            .iter()
            .chain(
                implicit_dependencies
                    .get(workspace_path)
                    .into_iter()
                    .flatten(),
            )
            .map(|dependency| stages.get(dependency).copied().unwrap_or(0) + 1)
            .max()
            .unwrap_or(0);
        stages.insert(workspace_path.clone(), stage);
    }

    let max_stage = stages.values().copied().max().unwrap_or(0);
    let mut levels = vec![Vec::new(); max_stage + 1];
    for workspace_path in &graph_context.topological_order {
        let stage = stages.get(workspace_path).copied().unwrap_or(0);
        levels[stage].push(workspace_path.clone());
    }
    Ok(levels)
}

fn implicit_local_remote_state_dependencies(
    request: &EngineRequest,
    repo_root: &Path,
    workspace_path: &str,
    environment_name: &str,
) -> Result<Vec<String>, EngineError> {
    let workspace_dir = repo_root.join(workspace_path);
    let prefix = format!(".yaffle/state/{environment_name}/");
    let suffix = "/terraform.tfstate";
    let mut dependencies = BTreeSet::new();

    for entry in fs::read_dir(&workspace_dir).map_err(|error| {
        request_error(
            request,
            "workspace_read_failed",
            format!(
                "Failed to inspect workspace '{}' for remote state references: {error}",
                workspace_path
            ),
        )
    })? {
        let entry = entry.map_err(|error| {
            request_error(
                request,
                "workspace_read_failed",
                format!(
                    "Failed to inspect workspace '{}' for remote state references: {error}",
                    workspace_path
                ),
            )
        })?;
        let path = entry.path();
        let is_tf_file = path.extension().and_then(|ext| ext.to_str()) == Some("tf");
        if !is_tf_file {
            continue;
        }

        let content = fs::read_to_string(&path).map_err(|error| {
            request_error(
                request,
                "workspace_read_failed",
                format!(
                    "Failed to read '{}' while scanning for remote state references: {error}",
                    path.display()
                ),
            )
        })?;

        let mut remaining = content.as_str();
        while let Some(start) = remaining.find(&prefix) {
            let after_prefix = &remaining[start + prefix.len()..];
            let Some(end) = after_prefix.find(suffix) else {
                break;
            };
            let dependency = after_prefix[..end].trim_matches('/');
            if !dependency.is_empty() && dependency != workspace_path {
                dependencies.insert(dependency.to_string());
            }
            remaining = &after_prefix[end + suffix.len()..];
        }
    }

    Ok(dependencies.into_iter().collect())
}

fn emit_progress_via_channel(
    progress_tx: &Option<mpsc::Sender<ConvergeWorkerMessage>>,
    event: EngineProgressEvent,
) {
    if let Some(tx) = progress_tx {
        let _ = tx.send(ConvergeWorkerMessage::Progress(event));
    }
}

fn execute_doctor_operation(request: &EngineRequest, working_dir: &Path) -> EngineResponse {
    let mut diagnostics = Vec::new();
    let mut summary_lines = Vec::new();
    let mut info_count = 0usize;
    let mut warning_count = 0usize;
    let mut error_count = 0usize;

    record_doctor_check(
        &mut diagnostics,
        &mut summary_lines,
        &mut info_count,
        &mut warning_count,
        &mut error_count,
        DiagnosticLevel::Info,
        "working_directory",
        format!("using working directory '{}'", working_dir.display()),
        None,
    );

    let tofu_report = inspect_tofu_resolution(&TofuResolutionRequest::default());
    match &tofu_report.selected {
        Some(resolution) => record_doctor_check(
            &mut diagnostics,
            &mut summary_lines,
            &mut info_count,
            &mut warning_count,
            &mut error_count,
            DiagnosticLevel::Info,
            "tofu_resolved",
            format!(
                "resolved tofu via {} at '{}' ({})",
                tofu_source_label(resolution.source),
                resolution.path.display(),
                resolution.version
            ),
            Some(BTreeMap::from([
                ("source".to_string(), json!(resolution.source)),
                (
                    "path".to_string(),
                    json!(resolution.path.display().to_string()),
                ),
                ("version".to_string(), json!(resolution.version)),
                ("attempts".to_string(), json!(tofu_report.attempts)),
            ])),
        ),
        None => record_doctor_check(
            &mut diagnostics,
            &mut summary_lines,
            &mut info_count,
            &mut warning_count,
            &mut error_count,
            DiagnosticLevel::Error,
            "tofu_resolution_failed",
            "could not resolve tofu using the configured source policy",
            Some(BTreeMap::from([(
                "attempts".to_string(),
                json!(tofu_report.attempts),
            )])),
        ),
    }

    let repo_context = match load_repo_context(working_dir, request) {
        Ok(repo_context) => repo_context,
        Err(error) => {
            record_doctor_check(
                &mut diagnostics,
                &mut summary_lines,
                &mut info_count,
                &mut warning_count,
                &mut error_count,
                DiagnosticLevel::Error,
                error.error.code,
                error.error.message,
                error.error.details,
            );

            return build_response(
                request,
                doctor_result_kind(error_count, warning_count),
                format_doctor_summary(info_count, warning_count, error_count, &summary_lines),
                Vec::new(),
                BTreeMap::new(),
                diagnostics,
            );
        }
    };

    record_doctor_check(
        &mut diagnostics,
        &mut summary_lines,
        &mut info_count,
        &mut warning_count,
        &mut error_count,
        DiagnosticLevel::Info,
        "config_loaded",
        format!(
            "loaded config from '{}'",
            repo_context.config_path.display()
        ),
        Some(BTreeMap::from([(
            "path".to_string(),
            json!(repo_context.config_path.display().to_string()),
        )])),
    );

    match &repo_context.current_namespace {
        Some(namespace) => record_doctor_check(
            &mut diagnostics,
            &mut summary_lines,
            &mut info_count,
            &mut warning_count,
            &mut error_count,
            DiagnosticLevel::Info,
            "repo_namespace_resolved",
            format!("inferred repo namespace '{}'", namespace),
            Some(BTreeMap::from([(
                "namespace".to_string(),
                json!(namespace),
            )])),
        ),
        None => record_doctor_check(
            &mut diagnostics,
            &mut summary_lines,
            &mut info_count,
            &mut warning_count,
            &mut error_count,
            DiagnosticLevel::Warning,
            "repo_namespace_unresolved",
            "could not infer repo namespace from git metadata; same-repo dependency filtering may be less strict",
            None,
        ),
    }

    let missing_workspace_directories = repo_context
        .config
        .workspaces
        .iter()
        .filter(|workspace| !repo_context.repo_root.join(&workspace.path).is_dir())
        .map(|workspace| workspace.path.clone())
        .collect::<Vec<_>>();

    if missing_workspace_directories.is_empty() {
        record_doctor_check(
            &mut diagnostics,
            &mut summary_lines,
            &mut info_count,
            &mut warning_count,
            &mut error_count,
            DiagnosticLevel::Info,
            "workspace_directories_present",
            format!(
                "found all {} configured workspace directories",
                repo_context.config.workspaces.len()
            ),
            Some(BTreeMap::from([(
                "workspace_count".to_string(),
                json!(repo_context.config.workspaces.len()),
            )])),
        );
    } else {
        record_doctor_check(
            &mut diagnostics,
            &mut summary_lines,
            &mut info_count,
            &mut warning_count,
            &mut error_count,
            DiagnosticLevel::Error,
            "workspace_directories_missing",
            format!(
                "{} configured workspace directories are missing",
                missing_workspace_directories.len()
            ),
            Some(BTreeMap::from([(
                "missing_workspaces".to_string(),
                json!(missing_workspace_directories),
            )])),
        );
    }

    match resolve_workspace_graph(
        &repo_context.repo_root,
        &repo_context.config,
        None,
        &WorkspaceGraphOptions {
            allowed_hosts: None,
            current_namespace: repo_context.current_namespace.clone(),
        },
    ) {
        Ok(graph) => record_doctor_check(
            &mut diagnostics,
            &mut summary_lines,
            &mut info_count,
            &mut warning_count,
            &mut error_count,
            DiagnosticLevel::Info,
            "static_graph_resolved",
            format!(
                "resolved static graph ({} workspaces, {} edges)",
                graph.workspaces.len(),
                graph_edge_count(&graph)
            ),
            Some(BTreeMap::from([
                ("workspace_count".to_string(), json!(graph.workspaces.len())),
                ("edge_count".to_string(), json!(graph_edge_count(&graph))),
            ])),
        ),
        Err(error) => record_graph_error(
            &mut diagnostics,
            &mut summary_lines,
            &mut info_count,
            &mut warning_count,
            &mut error_count,
            graph_error(request, error),
            Some("static repo graph".to_string()),
        ),
    }

    if repo_context.config.environments.is_empty() {
        record_doctor_check(
            &mut diagnostics,
            &mut summary_lines,
            &mut info_count,
            &mut warning_count,
            &mut error_count,
            DiagnosticLevel::Warning,
            "named_environments_missing",
            "config does not define any named environments",
            None,
        );
    } else {
        for environment in &repo_context.config.environments {
            match resolve_workspace_graph(
                &repo_context.repo_root,
                &repo_context.config,
                Some(&environment.name),
                &WorkspaceGraphOptions {
                    allowed_hosts: None,
                    current_namespace: repo_context.current_namespace.clone(),
                },
            ) {
                Ok(graph) => record_doctor_check(
                    &mut diagnostics,
                    &mut summary_lines,
                    &mut info_count,
                    &mut warning_count,
                    &mut error_count,
                    DiagnosticLevel::Info,
                    "environment_graph_resolved",
                    format!(
                        "resolved named environment '{}' ({} workspaces, {} edges)",
                        environment.name,
                        graph.workspaces.len(),
                        graph_edge_count(&graph)
                    ),
                    Some(BTreeMap::from([
                        ("environment".to_string(), json!(environment.name)),
                        ("workspace_count".to_string(), json!(graph.workspaces.len())),
                        ("edge_count".to_string(), json!(graph_edge_count(&graph))),
                    ])),
                ),
                Err(error) => record_graph_error(
                    &mut diagnostics,
                    &mut summary_lines,
                    &mut info_count,
                    &mut warning_count,
                    &mut error_count,
                    graph_error(
                        &EngineRequest {
                            operation: request.operation.clone(),
                            target: Some(EnvironmentTarget {
                                environment: environment.name.clone(),
                            }),
                            selection: WorkspaceSelection::default(),
                            wait_for: None,
                        },
                        error,
                    ),
                    Some(format!("environment '{}'", environment.name)),
                ),
            }
        }
    }

    build_response(
        request,
        doctor_result_kind(error_count, warning_count),
        format_doctor_summary(info_count, warning_count, error_count, &summary_lines),
        Vec::new(),
        BTreeMap::new(),
        diagnostics,
    )
}

fn load_repo_context(
    working_dir: &Path,
    request: &EngineRequest,
) -> Result<RepoContext, EngineError> {
    let config_path = find_yaffle_toml(working_dir).ok_or_else(|| {
        request_error(
            request,
            "config_not_found",
            format!(
                "Could not find `yaffle.toml` in '{}' or any parent directory.",
                working_dir.display()
            ),
        )
    })?;

    let raw = fs::read_to_string(&config_path).map_err(|error| {
        request_error(
            request,
            "config_read_failed",
            format!(
                "Failed to read '{}' as Yaffle config: {error}",
                config_path.display()
            ),
        )
    })?;

    let config = parse_yaffle_toml(&raw).map_err(|error| {
        request_error(
            request,
            "config_invalid",
            format!(
                "Invalid Yaffle config at '{}': {error}",
                config_path.display()
            ),
        )
    })?;

    let repo_root = config_path
        .parent()
        .expect("yaffle.toml should always have a parent directory")
        .to_path_buf();

    Ok(RepoContext {
        current_namespace: derive_current_namespace(&repo_root),
        repo_root,
        config_path,
        config,
    })
}

fn load_graph_context(
    repo_context: &RepoContext,
    request: &EngineRequest,
) -> Result<GraphContext, EngineError> {
    let graph = resolve_workspace_graph(
        &repo_context.repo_root,
        &repo_context.config,
        request
            .target
            .as_ref()
            .map(|target| target.environment.as_str()),
        &WorkspaceGraphOptions {
            allowed_hosts: None,
            current_namespace: repo_context.current_namespace.clone(),
        },
    )
    .map_err(|error| graph_error(request, error))?;

    let graph = apply_workspace_selection(&repo_context.config, &graph, &request.selection)
        .map_err(|error| graph_error(request, error))?;

    let topological_order = graph
        .topological_order()
        .map_err(|error| graph_error(request, error))?;

    Ok(GraphContext {
        graph,
        topological_order,
    })
}

fn validate_request(request: &EngineRequest) -> Result<(), EngineError> {
    let policy = operation_policy(&request.operation);

    match &request.target {
        Some(target) => validate_environment_name(&target.environment)
            .map_err(|error| request_error(request, "invalid_environment", error.to_string()))?,
        None if policy.requires_target => {
            return Err(request_error(
                request,
                "target_required",
                format!(
                    "Operation '{}' requires an environment target.",
                    operation_name(&request.operation)
                ),
            ));
        }
        None => {}
    }

    if request
        .selection
        .workspaces
        .iter()
        .any(|workspace| workspace.trim().is_empty())
    {
        return Err(request_error(
            request,
            "invalid_workspace_selection",
            "workspace selection entries must not be empty",
        ));
    }

    match policy.selection_mode {
        SelectionMode::Any => {}
        SelectionMode::None if !request.selection.is_empty() => {
            return Err(request_error(
                request,
                "workspace_selection_not_supported",
                format!(
                    "Operation '{}' does not support workspace selection.",
                    operation_name(&request.operation)
                ),
            ));
        }
        SelectionMode::None => {}
    }

    if request.operation == EngineOperation::Wait
        && request
            .wait_for
            .as_ref()
            .map(|value| value.trim())
            .unwrap_or("")
            .is_empty()
    {
        return Err(request_error(
            request,
            "invalid_condition",
            "condition passed to --for must not be empty",
        ));
    }

    Ok(())
}

fn operation_policy(operation: &EngineOperation) -> OperationPolicy {
    match operation {
        EngineOperation::Converge => OperationPolicy {
            requires_target: true,
            requires_repo_context: true,
            selection_mode: SelectionMode::Any,
        },
        EngineOperation::Destroy => OperationPolicy {
            requires_target: true,
            requires_repo_context: true,
            selection_mode: SelectionMode::Any,
        },
        EngineOperation::Status => OperationPolicy {
            requires_target: true,
            requires_repo_context: true,
            selection_mode: SelectionMode::Any,
        },
        EngineOperation::Wait => OperationPolicy {
            requires_target: true,
            requires_repo_context: true,
            selection_mode: SelectionMode::None,
        },
        EngineOperation::Outputs => OperationPolicy {
            requires_target: true,
            requires_repo_context: true,
            selection_mode: SelectionMode::Any,
        },
        EngineOperation::Graph => OperationPolicy {
            requires_target: false,
            requires_repo_context: true,
            selection_mode: SelectionMode::Any,
        },
        EngineOperation::Doctor => OperationPolicy {
            requires_target: false,
            requires_repo_context: false,
            selection_mode: SelectionMode::None,
        },
    }
}

fn operation_name(operation: &EngineOperation) -> &'static str {
    match operation {
        EngineOperation::Converge => "converge",
        EngineOperation::Destroy => "destroy",
        EngineOperation::Status => "status",
        EngineOperation::Wait => "wait",
        EngineOperation::Outputs => "outputs",
        EngineOperation::Graph => "graph",
        EngineOperation::Doctor => "doctor",
    }
}

fn repo_context_diagnostics(repo_context: &RepoContext) -> Vec<DiagnosticMessage> {
    let mut diagnostics = vec![DiagnosticMessage {
        level: DiagnosticLevel::Info,
        code: Some("config_loaded".to_string()),
        message: format!(
            "Loaded Yaffle config from '{}'.",
            repo_context.config_path.display()
        ),
        workspace_path: None,
        item_key: None,
        details: None,
    }];

    if repo_context.current_namespace.is_none() {
        diagnostics.push(DiagnosticMessage {
            level: DiagnosticLevel::Warning,
            code: Some("repo_namespace_unresolved".to_string()),
            message: "Could not infer the repo namespace from git metadata; same-repo dependency filtering may be less strict.".to_string(),
            workspace_path: None,
            item_key: None,
            details: None,
        });
    }

    diagnostics
}

fn module_api_host_override() -> Option<String> {
    env::var(MODULE_API_HOST_OVERRIDE_ENV_VAR)
        .ok()
        .map(|value| value.trim().to_string())
        .map(|value| strip_url_scheme(&value).to_string())
        .filter(|value| !value.is_empty() && value != CANONICAL_YAFFLE_MODULE_HOST)
}

fn effective_yaffle_module_host() -> String {
    module_api_host_override().unwrap_or_else(|| CANONICAL_YAFFLE_MODULE_HOST.to_string())
}

fn rewrite_canonical_yaffle_host_url(url: &str) -> String {
    let Some(override_host) = module_api_host_override() else {
        return url.to_string();
    };

    let Ok(mut parsed) = reqwest::Url::parse(url) else {
        return url.to_string();
    };
    if parsed.host_str() != Some(CANONICAL_YAFFLE_MODULE_HOST) {
        return url.to_string();
    }

    let (host, port) = override_host
        .split_once(':')
        .map(|(host, port)| (host, port.parse::<u16>().ok()))
        .unwrap_or((override_host.as_str(), None));
    if parsed.set_host(Some(host)).is_err() {
        return url.to_string();
    }
    if parsed.set_port(port).is_err() {
        return url.to_string();
    }

    parsed.to_string()
}

fn strip_url_scheme(value: &str) -> &str {
    value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .unwrap_or(value)
}

fn rewrite_workspace_module_hosts(
    request: &EngineRequest,
    prepared_repo: &PreparedExecutionRepo,
    workspace_path: &str,
) -> Result<(), EngineError> {
    let workspace_dir = prepared_repo.repo_root.join(workspace_path);
    let override_host = module_api_host_override();

    for entry in fs::read_dir(&workspace_dir).map_err(|error| {
        request_error(
            request,
            "workspace_read_failed",
            format!(
                "Failed to read workspace directory '{}': {error}",
                workspace_dir.display()
            ),
        )
    })? {
        let entry = entry.map_err(|error| {
            request_error(
                request,
                "workspace_read_failed",
                format!(
                    "Failed to inspect workspace directory '{}': {error}",
                    workspace_dir.display()
                ),
            )
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("tf") {
            continue;
        }

        let content = fs::read_to_string(&path).map_err(|error| {
            request_error(
                request,
                "workspace_read_failed",
                format!(
                    "Failed to read workspace file '{}': {error}",
                    path.display()
                ),
            )
        })?;
        let mut rendered = content;

        if let Some(override_host) = &override_host {
            if rendered.contains(CANONICAL_YAFFLE_MODULE_HOST) {
                rendered = rendered.replace(CANONICAL_YAFFLE_MODULE_HOST, override_host);
            }
        }

        fs::write(&path, rendered).map_err(|error| {
            request_error(
                request,
                "workspace_rewrite_failed",
                format!(
                    "Failed to rewrite module sources or module host in '{}': {error}",
                    path.display()
                ),
            )
        })?;
    }

    Ok(())
}

impl PreparedWorkspaceAuth {
    fn env_pairs(&self) -> Vec<(String, String)> {
        self.tf_cli_config_file
            .as_ref()
            .map(|path| vec![("TF_CLI_CONFIG_FILE".to_string(), path.display().to_string())])
            .unwrap_or_default()
    }

    fn tf_cli_config_file_path(&self) -> Option<String> {
        self.tf_cli_config_file
            .as_ref()
            .map(|path| path.display().to_string())
    }
}

fn append_auth_diagnostics(
    diagnostics: &mut Vec<DiagnosticMessage>,
    workspace_path: &str,
    auth: &PreparedWorkspaceAuth,
) {
    for resolved in &auth.resolved_hosts {
        diagnostics.push(DiagnosticMessage {
            level: DiagnosticLevel::Info,
            code: Some("auth_host_resolved".to_string()),
            message: format!(
                "Resolved local auth for host '{}' via {}.",
                resolved.host,
                auth_credential_source_name(resolved.source)
            ),
            workspace_path: Some(workspace_path.to_string()),
            item_key: None,
            details: Some(BTreeMap::from([
                ("host".to_string(), json!(resolved.host)),
                (
                    "source".to_string(),
                    json!(auth_credential_source_name(resolved.source)),
                ),
                (
                    "tf_cli_config_file".to_string(),
                    json!(auth.tf_cli_config_file_path()),
                ),
            ])),
        });
    }

    for host in &auth.missing_hosts {
        diagnostics.push(DiagnosticMessage {
            level: DiagnosticLevel::Warning,
            code: Some("auth_host_missing".to_string()),
            message: format!(
                "No local auth material was found for required host '{}'.",
                host
            ),
            workspace_path: Some(workspace_path.to_string()),
            item_key: None,
            details: Some(BTreeMap::from([
                ("host".to_string(), json!(host)),
                (
                    "expected_env_var".to_string(),
                    json!(host_token_env_var_name(host)),
                ),
            ])),
        });
    }
}

fn auth_credential_source_name(source: AuthCredentialSource) -> &'static str {
    match source {
        AuthCredentialSource::CliConfig => "cli_config",
        AuthCredentialSource::EnvToken => "env_token",
        AuthCredentialSource::YaffleExecutionToken => "yaffle_execution_token",
    }
}

fn prepare_workspace_auth(
    request: &EngineRequest,
    repo_context: &RepoContext,
    prepared_repo: &PreparedExecutionRepo,
    workspace_path: &str,
    workspace_dir: &Path,
    allow_scoped_yaffle_auth: bool,
) -> Result<PreparedWorkspaceAuth, EngineError> {
    let required_hosts = discover_workspace_auth_hosts(request, workspace_dir)?;
    if required_hosts.is_empty() {
        return Ok(PreparedWorkspaceAuth::default());
    }

    let existing_cli_config_path = discover_existing_cli_config_path();
    let existing_hosts = existing_cli_config_path
        .as_deref()
        .map(discover_cli_credentials_hosts)
        .unwrap_or_default();
    let mut host_tokens = required_hosts
        .iter()
        .filter_map(|host| {
            env::var(host_token_env_var_name(host))
                .ok()
                .map(|token| (host.clone(), token))
        })
        .collect::<BTreeMap<_, _>>();

    let mut execution_token_hosts = BTreeSet::new();
    if allow_scoped_yaffle_auth {
        let yaffle_host = effective_yaffle_module_host();
        if required_hosts.iter().any(|host| host == &yaffle_host) {
            let canonical_repo_namespace = repo_context.current_namespace.as_ref().ok_or_else(|| {
                request_error(
                    request,
                    "repo_namespace_unresolved",
                    "Could not infer repo namespace for hosted Yaffle module auth. Configure a canonical git remote before using local-first hosted modules.",
                )
            })?;
            let local_repo_fingerprint = compute_local_repo_fingerprint(&repo_context.repo_root)
                .map_err(|error| local_first_error(request, "repo_fingerprint_failed", error))?;
            let principal = ensure_anonymous_principal().map_err(|error| {
                local_first_error(request, "anonymous_session_bootstrap_failed", error)
            })?;
            let execution_credential = mint_execution_credential(
                &principal,
                &ExecutionCredentialRequest {
                    canonical_repo_namespace,
                    local_repo_fingerprint: &local_repo_fingerprint,
                    environment_name: request
                        .target
                        .as_ref()
                        .map(|target| target.environment.as_str())
                        .unwrap_or("unknown"),
                    consumer_workspace_path: workspace_path,
                    session_kind: ExecutionCredentialKind::WorkspaceInit,
                },
            )
            .map_err(|error| local_first_error(request, "execution_token_mint_failed", error))?;

            host_tokens.insert(yaffle_host.clone(), execution_credential.token);
            execution_token_hosts.insert(yaffle_host);
        }
    }

    let mut resolved_hosts = host_tokens
        .keys()
        .map(|host| ResolvedAuthHost {
            host: host.clone(),
            source: if execution_token_hosts.contains(host) {
                AuthCredentialSource::YaffleExecutionToken
            } else {
                AuthCredentialSource::EnvToken
            },
        })
        .collect::<Vec<_>>();
    resolved_hosts.extend(
        required_hosts
            .iter()
            .filter(|host| {
                existing_hosts.contains(host.as_str()) && !host_tokens.contains_key(*host)
            })
            .map(|host| ResolvedAuthHost {
                host: host.clone(),
                source: AuthCredentialSource::CliConfig,
            }),
    );
    resolved_hosts.sort_by(|left, right| left.host.cmp(&right.host));

    let missing_hosts = required_hosts
        .iter()
        .filter(|host| !existing_hosts.contains(host.as_str()) && !host_tokens.contains_key(*host))
        .cloned()
        .collect::<Vec<_>>();

    let tf_cli_config_file = if host_tokens.is_empty() {
        existing_cli_config_path
    } else {
        Some(write_workspace_cli_credentials_file(
            request,
            prepared_repo,
            workspace_path,
            existing_cli_config_path.as_deref(),
            &host_tokens,
        )?)
    };

    Ok(PreparedWorkspaceAuth {
        tf_cli_config_file,
        required_hosts,
        resolved_hosts,
        missing_hosts,
    })
}

fn discover_workspace_auth_hosts(
    request: &EngineRequest,
    workspace_dir: &Path,
) -> Result<Vec<String>, EngineError> {
    let body = load_workspace_hcl_body(request, workspace_dir)?;
    let mut hosts = BTreeSet::new();
    let mut references_module_registry_host_variable = false;

    for block in body.blocks() {
        match block.identifier() {
            "terraform" => {
                for nested in block.body().blocks() {
                    match nested.identifier() {
                        "cloud" => {
                            hosts.insert(
                                attribute_string_value(nested.body(), "hostname")
                                    .unwrap_or_else(|| "app.terraform.io".to_string()),
                            );
                        }
                        "backend" => {
                            let backend_kind = nested
                                .labels()
                                .first()
                                .map(|label| label.as_str())
                                .unwrap_or_default();
                            match backend_kind {
                                "remote" => {
                                    hosts.insert(
                                        attribute_string_value(nested.body(), "hostname")
                                            .unwrap_or_else(|| "app.terraform.io".to_string()),
                                    );
                                }
                                "http" => {
                                    if let Some(address) =
                                        attribute_string_value(nested.body(), "address")
                                    {
                                        if let Some(host) = extract_url_host(&address) {
                                            hosts.insert(host);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
            }
            "module" => {
                if let Some(source) = attribute_string_value(block.body(), "source") {
                    if let Some(host) = parse_module_source_auth_host(&source) {
                        hosts.insert(host);
                    }
                } else if module_source_uses_module_registry_host_variable(block.body()) {
                    references_module_registry_host_variable = true;
                }
            }
            _ => {}
        }
    }

    if references_module_registry_host_variable {
        hosts.insert(effective_yaffle_module_host());
    }

    Ok(hosts.into_iter().collect())
}

fn attribute_string_value(body: &Body, name: &str) -> Option<String> {
    body.attributes()
        .find(|attribute| attribute.key() == name)
        .and_then(|attribute| {
            attribute
                .expr()
                .evaluate(&HclContext::new())
                .ok()
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
        })
}

fn parse_module_source_auth_host(source: &str) -> Option<String> {
    if source.contains("://") {
        return None;
    }

    let parts = source.split('/').collect::<Vec<_>>();
    if parts.len() < 3 {
        return None;
    }

    let host = parts[0];
    let looks_like_host = (host.contains('.') || host.contains(':'))
        && host != "."
        && host != ".."
        && host
            .chars()
            .any(|character| character.is_ascii_alphanumeric());

    if looks_like_host {
        Some(host.to_string())
    } else {
        None
    }
}

fn module_source_uses_module_registry_host_variable(body: &Body) -> bool {
    body.attributes()
        .find(|attribute| attribute.key() == "source")
        .map(|attribute| {
            attribute
                .expr()
                .to_string()
                .contains("module_registry_host")
        })
        .unwrap_or(false)
}

fn extract_url_host(url: &str) -> Option<String> {
    let remainder = url.split_once("://")?.1;
    let authority = remainder.split('/').next()?;
    let host = authority.rsplit('@').next()?.trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

fn discover_existing_cli_config_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("TF_CLI_CONFIG_FILE").map(PathBuf::from) {
        if path.is_file() {
            return Some(path);
        }
    }

    let home = env::var_os("HOME").map(PathBuf::from)?;
    let mut candidates = vec![
        home.join(".terraformrc"),
        home.join(".terraform.d/credentials.tfrc.json"),
        home.join(".tofurc"),
        home.join(".opentofu.d/credentials.tfrc.json"),
    ];
    if let Some(xdg_config_home) = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        candidates.push(xdg_config_home.join("opentofu/tofurc"));
        candidates.push(xdg_config_home.join("opentofu/credentials.tfrc.json"));
    }

    candidates.into_iter().find(|path| path.is_file())
}

fn discover_cli_credentials_hosts(path: &Path) -> BTreeSet<String> {
    let Ok(content) = fs::read_to_string(path) else {
        return BTreeSet::new();
    };

    if path.extension().and_then(|value| value.to_str()) == Some("json")
        || content.trim_start().starts_with('{')
    {
        return parse_json_credentials_hosts(&content);
    }

    parse_hcl_credentials_hosts(&content)
}

fn parse_json_credentials_hosts(content: &str) -> BTreeSet<String> {
    serde_json::from_str::<Value>(content)
        .ok()
        .and_then(|value| value.get("credentials").and_then(Value::as_object).cloned())
        .map(|credentials| credentials.into_iter().map(|(host, _)| host).collect())
        .unwrap_or_default()
}

fn parse_hcl_credentials_hosts(content: &str) -> BTreeSet<String> {
    let Ok(body) = hcl::from_str::<Body>(content) else {
        return BTreeSet::new();
    };

    body.blocks()
        .filter(|block| block.identifier() == "credentials")
        .filter_map(|block| {
            block
                .labels()
                .first()
                .map(|label| label.as_str().to_string())
        })
        .collect()
}

fn write_workspace_cli_credentials_file(
    request: &EngineRequest,
    prepared_repo: &PreparedExecutionRepo,
    workspace_path: &str,
    base_config_path: Option<&Path>,
    env_token_hosts: &BTreeMap<String, String>,
) -> Result<PathBuf, EngineError> {
    let auth_dir = prepared_repo.repo_root.join(".yaffle/auth");
    fs::create_dir_all(&auth_dir).map_err(|error| {
        request_error(
            request,
            "auth_config_write_failed",
            format!(
                "Failed to create auth config directory for workspace '{}': {error}",
                workspace_path
            ),
        )
    })?;

    let output_path = auth_dir.join(format!("{}.tfrc", slugify_path(workspace_path)));

    write_cli_credentials_file(
        request,
        &output_path,
        workspace_path,
        base_config_path,
        env_token_hosts,
    )
}

fn write_tf_login_credentials_file(
    request: &EngineRequest,
    environment_name: &str,
    workspace_path: &str,
    host: &str,
    execution_credential: &ExecutionCredential,
) -> Result<PathBuf, EngineError> {
    let auth_dir = local_first::local_auth_store_path()
        .map_err(|error| local_first_error(request, "auth_config_write_failed", error))?
        .parent()
        .expect("local auth store path should have a parent")
        .join("execution");
    fs::create_dir_all(&auth_dir).map_err(|error| {
        request_error(
            request,
            "auth_config_write_failed",
            format!(
                "Failed to create auth config directory for workspace '{}': {error}",
                workspace_path
            ),
        )
    })?;

    let output_path = auth_dir.join(format!(
        "{}-{}.tfrc.json",
        slugify_path(environment_name),
        slugify_path(workspace_path)
    ));
    let host_tokens = BTreeMap::from([(host.to_string(), execution_credential.token.clone())]);

    write_cli_credentials_file(
        request,
        &output_path,
        workspace_path,
        discover_existing_cli_config_path().as_deref(),
        &host_tokens,
    )
}

fn write_cli_credentials_file(
    request: &EngineRequest,
    output_path: &Path,
    workspace_label: &str,
    base_config_path: Option<&Path>,
    host_tokens: &BTreeMap<String, String>,
) -> Result<PathBuf, EngineError> {
    let parent = output_path.parent().ok_or_else(|| {
        request_error(
            request,
            "auth_config_write_failed",
            format!(
                "Auth config path '{}' has no parent directory.",
                output_path.display()
            ),
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        request_error(
            request,
            "auth_config_write_failed",
            format!("Failed to create auth config directory: {error}"),
        )
    })?;
    #[cfg(unix)]
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|error| {
        request_error(
            request,
            "auth_config_write_failed",
            format!("Failed to secure auth config directory: {error}"),
        )
    })?;

    let base_content = base_config_path
        .and_then(|path| fs::read_to_string(path).ok())
        .unwrap_or_default();

    let content = if base_config_path
        .map(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .unwrap_or(false)
        || base_content.trim_start().starts_with('{')
        || base_config_path.is_none()
    {
        let mut credentials = base_config_path
            .and_then(|_| serde_json::from_str::<Value>(&base_content).ok())
            .and_then(|value| value.get("credentials").and_then(Value::as_object).cloned())
            .unwrap_or_default();

        for (host, token) in host_tokens {
            credentials.insert(host.clone(), json!({ "token": token }));
        }

        serde_json::to_string_pretty(&json!({ "credentials": credentials })).map_err(|error| {
            request_error(
                request,
                "auth_config_write_failed",
                format!(
                    "Failed to serialize auth config for workspace '{}': {error}",
                    workspace_label
                ),
            )
        })?
    } else {
        let mut content = base_content;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        for (host, token) in host_tokens {
            content.push_str(&format!(
                "\ncredentials \"{host}\" {{\n  token = \"{}\"\n}}\n",
                escape_hcl_string(token)
            ));
        }
        content
    };

    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        request_error(
            request,
            "auth_config_write_failed",
            format!(
                "Failed to prepare auth config for workspace '{}': {error}",
                workspace_label
            ),
        )
    })?;
    temporary.write_all(content.as_bytes()).map_err(|error| {
        request_error(
            request,
            "auth_config_write_failed",
            format!(
                "Failed to write auth config for workspace '{}': {error}",
                workspace_label
            ),
        )
    })?;
    temporary.as_file().sync_all().map_err(|error| {
        request_error(
            request,
            "auth_config_write_failed",
            format!(
                "Failed to sync auth config for workspace '{}': {error}",
                workspace_label
            ),
        )
    })?;
    temporary.persist(output_path).map_err(|error| {
        request_error(
            request,
            "auth_config_write_failed",
            format!(
                "Failed to install auth config for workspace '{}': {}",
                workspace_label, error.error
            ),
        )
    })?;

    Ok(output_path.to_path_buf())
}

fn escape_hcl_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn host_token_env_var_name(host: &str) -> String {
    format!(
        "TF_TOKEN_{}",
        host.chars()
            .map(|character| match character {
                '.' | ':' => '_',
                other => other,
            })
            .collect::<String>()
    )
}

fn classify_tofu_command_failure(
    error_code: &str,
    workspace_path: &str,
    stderr: &str,
    auth: &PreparedWorkspaceAuth,
) -> Option<(String, String)> {
    if error_code != "tofu_init_failed" {
        return None;
    }

    let primary_host = auth
        .required_hosts
        .first()
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());

    let normalized = stderr.to_ascii_lowercase();
    if normalized.contains("connect: connection refused")
        || normalized.contains("no such host")
        || normalized.contains("failed to request discovery document")
        || normalized.contains("timeout")
    {
        return Some((
            "auth_host_unreachable".to_string(),
            format!(
                "Required backend or registry host '{}' was unreachable while initializing workspace '{}': {}",
                primary_host, workspace_path, stderr
            ),
        ));
    }

    if normalized.contains("403 forbidden")
        || normalized.contains("401 unauthorized")
        || normalized.contains("unauthorized")
        || normalized.contains("forbidden")
    {
        return Some((
            "auth_host_unauthorized".to_string(),
            format!(
                "Local auth for required host '{}' was rejected while initializing workspace '{}': {}",
                primary_host, workspace_path, stderr
            ),
        ));
    }

    if let Some(host) = auth.missing_hosts.first() {
        return Some((
            "auth_host_missing".to_string(),
            format!(
                "No local auth material was found for required host '{}' while initializing workspace '{}': {}",
                host, workspace_path, stderr
            ),
        ));
    }

    None
}

fn derive_environment_materialization(observations: &[WorkspaceStatusObservation]) -> String {
    if observations
        .iter()
        .any(|observation| observation.materialization == "partially_present")
    {
        return "partially_present".to_string();
    }

    let present_count = observations
        .iter()
        .filter(|observation| observation.materialization == "present")
        .count();

    if present_count == 0 {
        "absent".to_string()
    } else if present_count == observations.len() {
        "present".to_string()
    } else {
        "partially_present".to_string()
    }
}

fn format_status_summary(
    environment_name: &str,
    observations: &[WorkspaceStatusObservation],
    environment_materialization: &str,
) -> String {
    let present_count = observations
        .iter()
        .filter(|observation| observation.materialization == "present")
        .count();
    let header = format!(
        "status for environment '{}': {} ({} of {} workspaces present)",
        environment_name,
        environment_materialization,
        present_count,
        observations.len()
    );

    if observations.is_empty() {
        return header;
    }

    let mut lines = vec![header, String::new()];
    for observation in observations {
        lines.push(format!(
            "- {}: {} ({} outputs)",
            observation.workspace_path,
            observation.materialization,
            observation.outputs.len()
        ));
    }

    lines.join("\n")
}

fn format_destroy_summary(environment_name: &str, workspace_paths: &[String]) -> String {
    let header = format!(
        "destroyed {} workspace(s) for environment '{}'",
        workspace_paths.len(),
        environment_name
    );

    if workspace_paths.is_empty() {
        return header;
    }

    let mut lines = vec![header, String::new()];
    lines.extend(
        workspace_paths
            .iter()
            .map(|workspace_path| format!("- {workspace_path}")),
    );
    lines.join("\n")
}

fn parse_wait_condition(value: &str) -> Option<WaitCondition> {
    match value {
        "infra_ready" => Some(WaitCondition::InfraReady),
        "activation_settled" => Some(WaitCondition::ActivationSettled),
        "verification_settled" => Some(WaitCondition::VerificationSettled),
        "usable" => Some(WaitCondition::Usable),
        "acceptable" => Some(WaitCondition::Acceptable),
        "teardown_settled" => Some(WaitCondition::TeardownSettled),
        _ => None,
    }
}

fn wait_condition_met(condition: WaitCondition, status_response: &EngineResponse) -> bool {
    let condition_name = match condition {
        WaitCondition::InfraReady => "infra_ready",
        WaitCondition::ActivationSettled => "activation_settled",
        WaitCondition::VerificationSettled => "verification_settled",
        WaitCondition::Usable => "usable",
        WaitCondition::Acceptable => "acceptable",
        WaitCondition::TeardownSettled => "teardown_settled",
    };
    if let Some(condition_value) = status_response
        .environment
        .as_ref()
        .and_then(|environment| {
            environment.conditions.iter().find(|condition| {
                condition.get("name").and_then(Value::as_str) == Some(condition_name)
            })
        })
        .and_then(|condition| condition.get("met").and_then(Value::as_bool))
    {
        return condition_value;
    }

    let materialization = status_response
        .environment
        .as_ref()
        .and_then(|environment| environment.materialization.as_deref());

    match condition {
        WaitCondition::TeardownSettled => materialization == Some("absent"),
        WaitCondition::Usable => materialization == Some("present"),
        WaitCondition::InfraReady
        | WaitCondition::ActivationSettled
        | WaitCondition::VerificationSettled
        | WaitCondition::Acceptable => {
            materialization == Some("present")
                && status_response.result.kind == OperationResultKind::Succeeded
        }
    }
}

fn wait_timeout() -> Duration {
    duration_from_env("YAFFLE_WAIT_TIMEOUT_MS", 30_000)
}

fn wait_poll_interval() -> Duration {
    duration_from_env("YAFFLE_WAIT_POLL_MS", 1_000)
}

fn duration_from_env(variable: &str, default_ms: u64) -> Duration {
    env::var(variable)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(default_ms))
}

fn prepare_execution_repo(
    source_repo_root: &Path,
    workspace_path: &str,
    request: &EngineRequest,
) -> Result<PreparedExecutionRepo, EngineError> {
    let temp_dir = tempfile::tempdir().map_err(|error| {
        request_error(
            request,
            "execution_workspace_prepare_failed",
            format!("Failed to create temporary execution directory: {error}"),
        )
    })?;
    let repo_root = temp_dir.path().to_path_buf();

    copy_repo_for_execution(source_repo_root, &repo_root).map_err(|error| {
        request_error(
            request,
            "execution_workspace_prepare_failed",
            format!(
                "Failed to prepare temporary execution repo from '{}': {error}",
                source_repo_root.display()
            ),
        )
    })?;

    let workspace_dir = repo_root.join(workspace_path);
    if !workspace_dir.is_dir() {
        return Err(request_error(
            request,
            "execution_workspace_missing",
            format!(
                "Selected workspace '{}' was not present in the prepared execution repo.",
                workspace_path
            ),
        ));
    }

    let tf_data_dir = repo_root
        .join(".yaffle-tf-data")
        .join(slugify_path(workspace_path));
    fs::create_dir_all(&tf_data_dir).map_err(|error| {
        request_error(
            request,
            "execution_workspace_prepare_failed",
            format!(
                "Failed to create TF_DATA_DIR for workspace '{}': {error}",
                workspace_path
            ),
        )
    })?;

    Ok(PreparedExecutionRepo {
        _temp_dir: Arc::new(temp_dir),
        repo_root,
        tf_data_dir,
    })
}

fn configure_workspace_execution(
    request: &EngineRequest,
    repo_context: &RepoContext,
    prepared_repo: &PreparedExecutionRepo,
    workspace: &yaffle_config::Workspace,
    environment_kind: EnvironmentKind,
) -> Result<PreparedWorkspaceExecution, EngineError> {
    if automatic_preview_isolation_requires_cloud_review(workspace, environment_kind) {
        return Err(request_error(
            request,
            "automatic_preview_isolation_cloud_review_required",
            format!(
                "Workspace '{}' enables automatic_preview_isolation for a transient environment. Unverified isolation review is available only in Yaffle Cloud; local execution will not bypass it.",
                workspace.path
            ),
        ));
    }

    let environment_name = request
        .target
        .as_ref()
        .map(|target| target.environment.clone())
        .unwrap_or_default();
    let workspace_dir = prepared_repo.repo_root.join(&workspace.path);

    rewrite_workspace_module_hosts(request, prepared_repo, &workspace.path)?;
    ensure_injected_variable_declarations(request, &workspace_dir)?;
    write_workspace_variables_file(
        request,
        &workspace_dir,
        workspace,
        &environment_name,
        environment_kind,
    )?;

    let has_explicit_backend = workspace_has_explicit_backend(request, &workspace_dir)?;
    if !has_explicit_backend {
        write_local_backend_override(request, prepared_repo, &workspace_dir, &workspace.path)?;
    }

    let auth = prepare_workspace_auth(
        request,
        repo_context,
        prepared_repo,
        &workspace.path,
        &workspace_dir,
        !has_explicit_backend,
    )?;

    Ok(PreparedWorkspaceExecution {
        uses_local_backend: !has_explicit_backend,
        auth,
    })
}

fn automatic_preview_isolation_requires_cloud_review(
    workspace: &yaffle_config::Workspace,
    environment_kind: EnvironmentKind,
) -> bool {
    workspace.automatic_preview_isolation && environment_kind == EnvironmentKind::Transient
}

fn run_tofu_command(
    request: &EngineRequest,
    tofu_resolution: &yaffle_tofu::TofuResolution,
    prepared_repo: &PreparedExecutionRepo,
    workspace_execution: &PreparedWorkspaceExecution,
    workspace_path: &str,
    args: &[&str],
    error_code: &'static str,
) -> Result<std::process::Output, EngineError> {
    run_tofu_command_internal(
        request,
        tofu_resolution,
        prepared_repo,
        workspace_execution,
        workspace_path,
        args,
        error_code,
        None,
    )
}

fn run_tofu_command_with_progress(
    request: &EngineRequest,
    tofu_resolution: &yaffle_tofu::TofuResolution,
    prepared_repo: &PreparedExecutionRepo,
    workspace_execution: &PreparedWorkspaceExecution,
    workspace_path: &str,
    args: &[&str],
    error_code: &'static str,
    reporter: &mut Option<&mut dyn EngineProgressReporter>,
) -> Result<std::process::Output, EngineError> {
    run_tofu_command_internal(
        request,
        tofu_resolution,
        prepared_repo,
        workspace_execution,
        workspace_path,
        args,
        error_code,
        Some(reporter),
    )
}

fn run_tofu_command_internal(
    request: &EngineRequest,
    tofu_resolution: &yaffle_tofu::TofuResolution,
    prepared_repo: &PreparedExecutionRepo,
    workspace_execution: &PreparedWorkspaceExecution,
    workspace_path: &str,
    args: &[&str],
    error_code: &'static str,
    mut reporter: Option<&mut Option<&mut dyn EngineProgressReporter>>,
) -> Result<std::process::Output, EngineError> {
    let workspace_dir = prepared_repo.repo_root.join(workspace_path);
    let tf_data_dir = prepared_repo.tf_data_dir.join(slugify_path(workspace_path));
    fs::create_dir_all(&tf_data_dir).map_err(|error| {
        request_error(
            request,
            "execution_workspace_prepare_failed",
            format!(
                "Failed to create TF_DATA_DIR for workspace '{}': {error}",
                workspace_path
            ),
        )
    })?;

    let output = if let Some(reporter_ref) = reporter.as_mut() {
        run_tofu_command_streaming(
            request,
            tofu_resolution,
            &workspace_dir,
            &tf_data_dir,
            workspace_execution,
            workspace_path,
            args,
            error_code,
            &prepared_repo.repo_root,
            reporter_ref,
        )?
    } else {
        tofu_resolution
            .command()
            .current_dir(&workspace_dir)
            .env("TF_DATA_DIR", &tf_data_dir)
            .env("TF_IN_AUTOMATION", "1")
            .env("TOFU_IN_AUTOMATION", "1")
            .envs(workspace_execution.auth.env_pairs())
            .args(args)
            .output()
            .map_err(|error| {
                request_error_with_details(
                    request,
                    error_code,
                    format!(
                        "Failed to execute tofu command '{}' for workspace '{}': {error}",
                        args.join(" "),
                        workspace_dir.display()
                    ),
                    Some(BTreeMap::from([
                        (
                            "workspace_dir".to_string(),
                            json!(workspace_dir.display().to_string()),
                        ),
                        (
                            "tf_cli_config_file".to_string(),
                            json!(workspace_execution.auth.tf_cli_config_file_path()),
                        ),
                        (
                            "repo_root".to_string(),
                            json!(prepared_repo.repo_root.display().to_string()),
                        ),
                        ("args".to_string(), json!(args)),
                    ])),
                )
            })?
    };

    if !output.status.success() {
        let stderr = utf8_trimmed(&output.stderr);
        let (failure_code, failure_message) = classify_tofu_command_failure(
            error_code,
            workspace_path,
            &stderr,
            &workspace_execution.auth,
        )
        .unwrap_or_else(|| {
            (
                error_code.to_string(),
                format!(
                    "tofu {} failed for workspace '{}': {}",
                    args.join(" "),
                    workspace_dir.display(),
                    stderr
                ),
            )
        });

        return Err(request_error_with_details(
            request,
            failure_code,
            failure_message,
            Some(BTreeMap::from([
                ("workspace_path".to_string(), json!(workspace_path)),
                (
                    "workspace_dir".to_string(),
                    json!(workspace_dir.display().to_string()),
                ),
                (
                    "tf_cli_config_file".to_string(),
                    json!(workspace_execution.auth.tf_cli_config_file_path()),
                ),
                (
                    "repo_root".to_string(),
                    json!(prepared_repo.repo_root.display().to_string()),
                ),
                ("args".to_string(), json!(args)),
                (
                    "exit_status".to_string(),
                    json!(output.status.code().unwrap_or_default()),
                ),
                ("stderr".to_string(), json!(stderr)),
                (
                    "required_auth_hosts".to_string(),
                    json!(workspace_execution.auth.required_hosts),
                ),
                (
                    "missing_auth_hosts".to_string(),
                    json!(workspace_execution.auth.missing_hosts),
                ),
            ])),
        ));
    }

    Ok(output)
}

fn run_tofu_command_streaming(
    request: &EngineRequest,
    tofu_resolution: &yaffle_tofu::TofuResolution,
    workspace_dir: &Path,
    tf_data_dir: &Path,
    workspace_execution: &PreparedWorkspaceExecution,
    workspace_path: &str,
    args: &[&str],
    error_code: &'static str,
    repo_root: &Path,
    reporter: &mut Option<&mut dyn EngineProgressReporter>,
) -> Result<std::process::Output, EngineError> {
    let mut child = tofu_resolution
        .command()
        .current_dir(workspace_dir)
        .env("TF_DATA_DIR", tf_data_dir)
        .env("TF_IN_AUTOMATION", "1")
        .env("TOFU_IN_AUTOMATION", "1")
        .envs(workspace_execution.auth.env_pairs())
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            request_error_with_details(
                request,
                error_code,
                format!(
                    "Failed to execute tofu command '{}' for workspace '{}': {error}",
                    args.join(" "),
                    workspace_dir.display()
                ),
                Some(BTreeMap::from([
                    (
                        "workspace_dir".to_string(),
                        json!(workspace_dir.display().to_string()),
                    ),
                    (
                        "tf_cli_config_file".to_string(),
                        json!(workspace_execution.auth.tf_cli_config_file_path()),
                    ),
                    (
                        "repo_root".to_string(),
                        json!(repo_root.display().to_string()),
                    ),
                    ("args".to_string(), json!(args)),
                ])),
            )
        })?;

    let stdout = child.stdout.take().expect("child stdout should be piped");
    let stderr = child.stderr.take().expect("child stderr should be piped");
    let (line_tx, line_rx) = std::sync::mpsc::channel::<(TofuLogStream, String)>();

    let stdout_reader = spawn_tofu_log_reader(stdout, TofuLogStream::Stdout, line_tx.clone());
    let stderr_reader = spawn_tofu_log_reader(stderr, TofuLogStream::Stderr, line_tx);

    let status = loop {
        match child.try_wait().map_err(|error| {
            request_error_with_details(
                request,
                error_code,
                format!(
                    "Failed to wait for tofu command '{}' in workspace '{}': {error}",
                    args.join(" "),
                    workspace_dir.display()
                ),
                Some(BTreeMap::from([(
                    "workspace_path".to_string(),
                    json!(workspace_path),
                )])),
            )
        })? {
            Some(status) => break status,
            None => {
                while let Ok((stream, line)) = line_rx.recv_timeout(Duration::from_millis(40)) {
                    emit_progress(
                        reporter,
                        EngineProgressEvent::TofuLog {
                            workspace_path: workspace_path.to_string(),
                            stream,
                            line,
                        },
                    );
                }
            }
        }
    };

    let stdout_bytes = stdout_reader
        .join()
        .expect("stdout reader should finish")
        .map_err(|error| {
            request_error(
                request,
                error_code,
                format!(
                    "Failed to read tofu stdout for workspace '{}': {error}",
                    workspace_path
                ),
            )
        })?;
    let stderr_bytes = stderr_reader
        .join()
        .expect("stderr reader should finish")
        .map_err(|error| {
            request_error(
                request,
                error_code,
                format!(
                    "Failed to read tofu stderr for workspace '{}': {error}",
                    workspace_path
                ),
            )
        })?;

    for (stream, line) in line_rx.try_iter() {
        emit_progress(
            reporter,
            EngineProgressEvent::TofuLog {
                workspace_path: workspace_path.to_string(),
                stream,
                line,
            },
        );
    }

    Ok(std::process::Output {
        status,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
    })
}

fn spawn_tofu_log_reader<R: io::Read + Send + 'static>(
    reader: R,
    stream: TofuLogStream,
    sender: std::sync::mpsc::Sender<(TofuLogStream, String)>,
) -> thread::JoinHandle<io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut buffer = Vec::new();
        let mut reader = BufReader::new(reader);
        let mut line = Vec::new();

        loop {
            line.clear();
            let bytes_read = reader.read_until(b'\n', &mut line)?;
            if bytes_read == 0 {
                break;
            }

            buffer.extend_from_slice(&line);
            let rendered = String::from_utf8_lossy(&line).trim().to_string();
            if !rendered.is_empty() {
                let _ = sender.send((stream, rendered));
            }
        }

        Ok(buffer)
    })
}

fn inspect_workspace_status(
    request: &EngineRequest,
    prepared_repo: &PreparedExecutionRepo,
    tofu_resolution: &yaffle_tofu::TofuResolution,
    workspace_execution: &PreparedWorkspaceExecution,
    workspace_path: &str,
) -> Result<WorkspaceStatusObservation, EngineError> {
    let environment_name = request
        .target
        .as_ref()
        .map(|target| target.environment.as_str())
        .unwrap_or("unknown");
    let state_present = if workspace_execution.uses_local_backend {
        local_backend_state_path(&prepared_repo.repo_root, environment_name, workspace_path)
            .is_file()
    } else {
        probe_nonlocal_state_presence(
            request,
            tofu_resolution,
            prepared_repo,
            workspace_execution,
            workspace_path,
        )?
    };

    if !state_present {
        return Ok(WorkspaceStatusObservation {
            workspace_path: workspace_path.to_string(),
            materialization: "absent".to_string(),
            outputs: BTreeMap::new(),
        });
    }

    let output = run_tofu_command(
        request,
        tofu_resolution,
        prepared_repo,
        workspace_execution,
        workspace_path,
        &["output", "-json", "-no-color"],
        "tofu_output_failed",
    )?;
    let outputs = parse_terraform_outputs(request, workspace_path, &output.stdout)?;

    Ok(WorkspaceStatusObservation {
        workspace_path: workspace_path.to_string(),
        materialization: "present".to_string(),
        outputs,
    })
}

fn probe_nonlocal_state_presence(
    request: &EngineRequest,
    tofu_resolution: &yaffle_tofu::TofuResolution,
    prepared_repo: &PreparedExecutionRepo,
    workspace_execution: &PreparedWorkspaceExecution,
    workspace_path: &str,
) -> Result<bool, EngineError> {
    let workspace_dir = prepared_repo.repo_root.join(workspace_path);
    let tf_data_dir = prepared_repo.tf_data_dir.join(slugify_path(workspace_path));
    fs::create_dir_all(&tf_data_dir).map_err(|error| {
        request_error(
            request,
            "execution_workspace_prepare_failed",
            format!(
                "Failed to create TF_DATA_DIR for workspace '{}': {error}",
                workspace_path
            ),
        )
    })?;

    let output = tofu_resolution
        .command()
        .current_dir(&workspace_dir)
        .env("TF_DATA_DIR", &tf_data_dir)
        .env("TF_IN_AUTOMATION", "1")
        .env("TOFU_IN_AUTOMATION", "1")
        .envs(workspace_execution.auth.env_pairs())
        .args(["state", "pull"])
        .output()
        .map_err(|error| {
            request_error(
                request,
                "tofu_state_pull_failed",
                format!(
                    "Failed to execute tofu state pull for workspace '{}': {error}",
                    workspace_path
                ),
            )
        })?;

    if output.status.success() {
        return Ok(true);
    }

    let stderr = utf8_trimmed(&output.stderr);
    if stderr.contains("Unable to find remote state")
        || stderr.contains("No stored state was found")
        || stderr.contains("No state file was found")
    {
        return Ok(false);
    }

    Err(request_error_with_details(
        request,
        "tofu_state_pull_failed",
        format!(
            "tofu state pull failed for workspace '{}': {}",
            workspace_path, stderr
        ),
        Some(BTreeMap::from([
            ("workspace_path".to_string(), json!(workspace_path)),
            ("stderr".to_string(), json!(stderr)),
        ])),
    ))
}

fn parse_terraform_outputs(
    request: &EngineRequest,
    workspace_path: &str,
    stdout: &[u8],
) -> Result<BTreeMap<String, TerraformOutput>, EngineError> {
    let raw_outputs = serde_json::from_slice::<BTreeMap<String, RawTerraformOutput>>(stdout)
        .map_err(|error| {
            request_error_with_details(
                request,
                "outputs_parse_failed",
                format!(
                    "Failed to parse tofu output JSON for workspace '{}': {error}",
                    workspace_path
                ),
                Some(BTreeMap::from([(
                    "stdout".to_string(),
                    json!(String::from_utf8_lossy(stdout).to_string()),
                )])),
            )
        })?;

    Ok(raw_outputs
        .into_iter()
        .map(|(name, output)| {
            (
                name,
                TerraformOutput {
                    value: output.value,
                    type_name: output.type_name.and_then(terraform_output_type_name),
                    sensitive: output.sensitive,
                },
            )
        })
        .collect())
}

fn format_outputs_summary(
    environment_name: &str,
    workspace_order: &[String],
    workspace_outputs: &BTreeMap<String, BTreeMap<String, TerraformOutput>>,
) -> String {
    if workspace_order.len() == 1 {
        let workspace_path = workspace_order
            .first()
            .expect("single-workspace outputs summary should have one workspace");
        let outputs = workspace_outputs
            .get(workspace_path)
            .cloned()
            .unwrap_or_default();
        let header = format!(
            "resolved {} output(s) for '{}' in environment '{}'",
            outputs.len(),
            workspace_path,
            environment_name
        );

        if outputs.is_empty() {
            return header;
        }

        let mut lines = vec![header, String::new()];
        for (name, output) in outputs {
            lines.push(format!("{name} = {}", render_output_value(&output)));
        }

        return lines.join("\n");
    }

    let output_count = workspace_outputs.values().map(BTreeMap::len).sum::<usize>();
    let header = format!(
        "resolved {} output(s) across {} workspace(s) in environment '{}'",
        output_count,
        workspace_order.len(),
        environment_name
    );

    if workspace_order.is_empty() {
        return header;
    }

    let mut lines = vec![header];
    for workspace_path in workspace_order {
        lines.push(String::new());
        lines.push(format!("{workspace_path}:"));
        let outputs = workspace_outputs
            .get(workspace_path)
            .cloned()
            .unwrap_or_default();
        if outputs.is_empty() {
            lines.push("  (no outputs)".to_string());
            continue;
        }

        for (name, output) in outputs {
            lines.push(format!("  {name} = {}", render_output_value(&output)));
        }
    }

    lines.join("\n")
}

fn render_output_value(output: &TerraformOutput) -> String {
    if output.sensitive == Some(true) {
        "<sensitive>".to_string()
    } else {
        serde_json::to_string(&output.value).unwrap_or_else(|_| "<unserializable>".to_string())
    }
}

fn select_workspace_outputs(
    request: &EngineRequest,
    workspace: &yaffle_config::Workspace,
    outputs: &BTreeMap<String, TerraformOutput>,
) -> Result<BTreeMap<String, TerraformOutput>, EngineError> {
    let mut selected = BTreeMap::new();
    let mut sensitive = Vec::new();
    let mut invalid = Vec::new();

    for name in workspace.outputs.keys() {
        let Some(output) = outputs.get(name) else {
            continue;
        };
        match output.sensitive {
            Some(true) => {
                sensitive.push(name.clone());
                continue;
            }
            Some(false) => {}
            None => {
                invalid.push(name.clone());
                continue;
            }
        }
        selected.insert(name.clone(), output.clone());
    }

    if !invalid.is_empty() {
        return Err(request_error(
            request,
            "invalid_output_metadata",
            format!(
                "Terraform outputs require explicit sensitivity metadata: {}",
                invalid.join(", ")
            ),
        ));
    }

    if !sensitive.is_empty() {
        return Err(request_error(
            request,
            "sensitive_output_not_allowed",
            format!(
                "Sensitive Terraform outputs cannot cross this trust boundary: {}. Store the secret in a secret manager and export only its ARN or identifier.",
                sensitive.join(", ")
            ),
        ));
    }

    Ok(selected)
}

fn redact_sensitive_outputs(
    request: &EngineRequest,
    outputs: &BTreeMap<String, TerraformOutput>,
) -> Result<BTreeMap<String, TerraformOutput>, EngineError> {
    let mut safe_outputs = BTreeMap::new();
    let mut invalid = Vec::new();

    for (name, output) in outputs {
        let mut safe_output = output.clone();
        match output.sensitive {
            Some(true) => safe_output.value = Value::Null,
            Some(false) => {}
            None => {
                invalid.push(name.clone());
                continue;
            }
        }
        safe_outputs.insert(name.clone(), safe_output);
    }

    if !invalid.is_empty() {
        return Err(request_error(
            request,
            "invalid_output_metadata",
            format!(
                "Terraform outputs require explicit sensitivity metadata: {}",
                invalid.join(", ")
            ),
        ));
    }

    Ok(safe_outputs)
}

fn maybe_publish_hosted_output_module(
    request: &EngineRequest,
    repo_context: &RepoContext,
    workspace_path: &str,
    selected_output_names: &[String],
    outputs: &BTreeMap<String, TerraformOutput>,
) -> Result<Option<String>, EngineError> {
    if selected_output_names.is_empty() {
        return Ok(None);
    }

    let canonical_repo_namespace = repo_context.current_namespace.as_ref().ok_or_else(|| {
        request_error(
            request,
            "repo_namespace_unresolved",
            "Could not infer repo namespace for hosted Yaffle output module publication. Configure a canonical git remote before using local-first hosted modules.",
        )
    })?;
    let principal = ensure_anonymous_principal()
        .map_err(|error| local_first_error(request, "anonymous_session_bootstrap_failed", error))?;
    let local_repo_fingerprint = compute_local_repo_fingerprint(&repo_context.repo_root)
        .map_err(|error| local_first_error(request, "repo_fingerprint_failed", error))?;
    let outputs_json = terraform_outputs_json(request, outputs)?;
    let published = publish_hosted_output_module(
        &principal,
        &HostedOutputModulePublishRequest {
            canonical_repo_namespace,
            local_repo_fingerprint: &local_repo_fingerprint,
            environment_name: request
                .target
                .as_ref()
                .map(|target| target.environment.as_str())
                .unwrap_or("unknown"),
            workspace_path,
            selected_output_names,
            outputs: &outputs_json,
        },
    )
    .map_err(|error| local_first_error(request, "hosted_output_module_publish_failed", error))?;

    Ok(Some(published.version))
}

#[derive(Debug, Clone)]
struct LifecycleDispatchContext {
    principal: StoredPrincipalCredential,
    canonical_repo_namespace: String,
    run_id: String,
}

fn lifecycle_hooks_for_environment<'a>(
    workspace: &'a yaffle_config::Workspace,
    environment_name: &str,
    phase: &str,
) -> Vec<&'a LifecycleHook> {
    let hooks = match phase {
        "activation" => &workspace.activation,
        "verification" => &workspace.verification,
        _ => return Vec::new(),
    };

    hooks
        .iter()
        .filter(|hook| environment_name_matches_patterns(environment_name, &hook.environments))
        .collect()
}

fn selected_workspaces_have_lifecycle_hooks(
    repo_context: &RepoContext,
    environment_name: &str,
    workspace_paths: &[String],
) -> bool {
    workspace_paths.iter().any(|workspace_path| {
        repo_context
            .config
            .workspaces
            .iter()
            .find(|workspace| workspace.path == *workspace_path)
            .map(|workspace| {
                !lifecycle_hooks_for_environment(workspace, environment_name, "activation")
                    .is_empty()
                    || !lifecycle_hooks_for_environment(workspace, environment_name, "verification")
                        .is_empty()
            })
            .unwrap_or(false)
    })
}

fn preflight_environment_governance(
    request: &EngineRequest,
    repo_context: &RepoContext,
    environment_name: &str,
) -> Result<(), EngineError> {
    let canonical_repo_namespace = repo_context.current_namespace.as_ref().ok_or_else(|| {
        request_error(
            request,
            "repo_namespace_unresolved",
            "Could not infer repo namespace for lifecycle governance. Configure a canonical git remote before converging lifecycle-managed environments.",
        )
    })?;
    let principal = ensure_anonymous_principal()
        .map_err(|error| local_first_error(request, "lifecycle_principal_failed", error))?;
    let local_repo_fingerprint = compute_local_repo_fingerprint(&repo_context.repo_root)
        .map_err(|error| local_first_error(request, "repo_fingerprint_failed", error))?;
    let decision = check_lifecycle_admission(
        &principal,
        &LifecycleAdmissionRequest {
            canonical_repo_namespace,
            local_repo_fingerprint: &local_repo_fingerprint,
            environment_name,
            execution_mode: "local",
        },
    )
    .map_err(|error| local_first_error(request, "lifecycle_admission_failed", error))?;

    if !decision.allowed {
        return Err(request_error(
            request,
            "environment_governance_blocked",
            decision.reason.unwrap_or_else(|| {
                format!(
                    "Environment '{}' is blocked by lifecycle governance policy in this execution context.",
                    environment_name
                )
            }),
        ));
    }

    Ok(())
}

fn ensure_lifecycle_dispatch_context(
    request: &EngineRequest,
    repo_context: &RepoContext,
    environment_name: &str,
    run_context: &mut Option<LifecycleDispatchContext>,
) -> Result<LifecycleDispatchContext, EngineError> {
    if let Some(existing) = run_context.clone() {
        return Ok(existing);
    }

    let canonical_repo_namespace = repo_context.current_namespace.as_ref().ok_or_else(|| {
        request_error(
            request,
            "repo_namespace_unresolved",
            "Could not infer repo namespace for lifecycle orchestration. Configure a canonical git remote before running activation or verification items.",
        )
    })?;
    let principal = ensure_anonymous_principal()
        .map_err(|error| local_first_error(request, "lifecycle_principal_failed", error))?;
    let local_repo_fingerprint = compute_local_repo_fingerprint(&repo_context.repo_root)
        .map_err(|error| local_first_error(request, "repo_fingerprint_failed", error))?;
    let run = create_lifecycle_run(
        &principal,
        &LifecycleRunRequest {
            canonical_repo_namespace,
            local_repo_fingerprint: &local_repo_fingerprint,
            environment_name,
            execution_mode: "local",
        },
    )
    .map_err(|error| local_first_error(request, "lifecycle_run_create_failed", error))?;

    let created = LifecycleDispatchContext {
        principal,
        canonical_repo_namespace: canonical_repo_namespace.clone(),
        run_id: run.id,
    };
    *run_context = Some(created.clone());
    Ok(created)
}

fn execute_lifecycle_hooks_for_workspace(
    request: &EngineRequest,
    repo_context: &RepoContext,
    workspace: &yaffle_config::Workspace,
    workspace_path: &str,
    outputs: &BTreeMap<String, TerraformOutput>,
    run_context: &mut Option<LifecycleDispatchContext>,
    diagnostics: &mut Vec<DiagnosticMessage>,
    phase: &str,
) -> Result<Vec<LifecycleItemResult>, EngineError> {
    let environment_name = request
        .target
        .as_ref()
        .map(|target| target.environment.as_str())
        .unwrap_or("unknown");
    let hooks = lifecycle_hooks_for_environment(workspace, environment_name, phase);
    if hooks.is_empty() {
        return Ok(Vec::new());
    }

    let context =
        ensure_lifecycle_dispatch_context(request, repo_context, environment_name, run_context)?;
    let selected_outputs = select_workspace_outputs(request, workspace, outputs)?;
    let outputs_json = terraform_outputs_json(request, &selected_outputs)?;
    let selected_output_names = workspace.outputs.keys().cloned().collect::<Vec<_>>();
    let mut results = Vec::new();

    for hook in hooks {
        let destination_url = lifecycle_destination_url(hook, &context.canonical_repo_namespace)?;
        let destination_class =
            lifecycle_destination_class(hook, &context.canonical_repo_namespace)?;
        let item = create_lifecycle_item(
            &context.principal,
            &LifecycleItemRequest {
                run_id: &context.run_id,
                workspace_path,
                key: &hook.key,
                phase,
                failure_policy: lifecycle_failure_policy_name(hook.failure),
                scopes: &hook.scopes,
                destination_url: &destination_url,
                destination_class: &destination_class,
                dispatch_mode: "local",
                selected_output_names: &selected_output_names,
                summary: Some(match phase {
                    "activation" => "Waiting for activation webhook completion",
                    "verification" => "Waiting for verification webhook completion",
                    _ => "Waiting for lifecycle webhook completion",
                }),
                metadata: &serde_json::Map::new(),
                callback_ttl_minutes: 60,
            },
        )
        .map_err(|error| local_first_error(request, "lifecycle_item_create_failed", error))?;

        if item.state == "blocked" || item.on_completion_url.is_none() {
            diagnostics.push(DiagnosticMessage {
                level: match hook.failure {
                    LifecycleFailurePolicy::Failed => DiagnosticLevel::Error,
                    LifecycleFailurePolicy::Degraded => DiagnosticLevel::Warning,
                },
                code: Some(format!("{}_item_blocked", phase)),
                message: format!(
                    "{} item '{}' was blocked before dispatch.",
                    title_case_phase(phase),
                    hook.key
                ),
                workspace_path: Some(workspace_path.to_string()),
                item_key: Some(hook.key.clone()),
                details: Some(BTreeMap::from([("state".to_string(), json!(item.state))])),
            });

            results.push(LifecycleItemResult {
                state: item.state,
                failure_policy: hook.failure,
            });
            continue;
        }

        let dispatch_result = dispatch_lifecycle_webhook(
            request,
            &context,
            &repo_context.repo_root,
            workspace_path,
            environment_name,
            phase,
            hook,
            &item,
            &outputs_json,
        );

        if let Err(error) = dispatch_result {
            let _ = report_lifecycle_failure(
                item.on_completion_url.as_deref().unwrap_or_default(),
                error.error.message.clone(),
            );
            return Err(error);
        }

        let final_item = wait_for_lifecycle_item_settlement(
            &context.principal,
            &item.id,
            hook.timeout.as_deref(),
        )
        .map_err(|error| local_first_error(request, "lifecycle_item_wait_failed", error))?;

        diagnostics.push(DiagnosticMessage {
            level: match final_item.state.as_str() {
                "succeeded" => DiagnosticLevel::Info,
                "degraded" => DiagnosticLevel::Warning,
                _ => DiagnosticLevel::Error,
            },
            code: Some("activation_item_settled".to_string()),
            message: format!(
                "{} item '{}' settled with state '{}'.",
                title_case_phase(phase),
                final_item.key,
                final_item.state
            ),
            workspace_path: Some(workspace_path.to_string()),
            item_key: Some(final_item.key.clone()),
            details: Some(BTreeMap::from([
                ("phase".to_string(), json!(final_item.phase)),
                ("state".to_string(), json!(final_item.state)),
                ("summary".to_string(), json!(final_item.summary)),
                ("reason".to_string(), json!(final_item.reason)),
            ])),
        });

        results.push(LifecycleItemResult {
            state: final_item.state,
            failure_policy: hook.failure,
        });
    }

    Ok(results)
}

#[derive(Debug, Clone)]
struct LifecycleItemResult {
    state: String,
    failure_policy: LifecycleFailurePolicy,
}

#[derive(Default)]
struct LifecyclePhaseVector {
    pending: u64,
    running: u64,
    succeeded: u64,
    degraded: u64,
    blocked: u64,
    failed: u64,
}

fn dispatch_lifecycle_webhook(
    request: &EngineRequest,
    context: &LifecycleDispatchContext,
    repo_root: &Path,
    workspace_path: &str,
    environment_name: &str,
    phase: &str,
    hook: &LifecycleHook,
    item: &crate::local_first::LifecycleItemHandle,
    outputs_json: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), EngineError> {
    let payload = lifecycle_dispatch_payload(
        context,
        repo_root,
        workspace_path,
        environment_name,
        phase,
        &hook.key,
        outputs_json,
        item.on_completion_url.clone(),
    );
    if lifecycle_dispatch_requires_control_plane(hook) {
        let dispatch_body = lifecycle_control_plane_dispatch_body(
            hook,
            payload,
            &context.run_id,
            &item.id,
            environment_name,
            workspace_path,
            phase,
        )?;
        dispatch_lifecycle_via_control_plane(&context.principal, &dispatch_body)
            .map_err(|error| local_first_error(request, "webhook_dispatch_failed", error))?;
        return Ok(());
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| request_error(request, "webhook_dispatch_failed", error.to_string()))?;
    let (dispatch_url, body, headers) =
        prepare_local_lifecycle_dispatch_request(request, hook, &payload)?;

    let response = client
        .post(dispatch_url)
        .headers(headers)
        .body(body)
        .send()
        .map_err(|error| {
            request_error(
                request,
                "webhook_dispatch_failed",
                format!(
                    "Failed to dispatch lifecycle webhook '{}': {error}",
                    hook.key
                ),
            )
        })?;

    if !response.status().is_success() {
        return Err(request_error(
            request,
            "webhook_dispatch_failed",
            format!(
                "Lifecycle webhook '{}' returned {}.",
                hook.key,
                response.status()
            ),
        ));
    }

    Ok(())
}

fn prepare_local_lifecycle_dispatch_request(
    request: &EngineRequest,
    hook: &LifecycleHook,
    payload: &Value,
) -> Result<(String, Vec<u8>, HeaderMap), EngineError> {
    let LifecycleHookDispatch::Generic(webhook) = &hook.dispatch else {
        return Err(request_error(
            request,
            "lifecycle_dispatch_invalid",
            format!(
                "Lifecycle hook '{}' requires control-plane-backed dispatch.",
                hook.key
            ),
        ));
    };

    let body = serde_json::to_vec(payload).map_err(|error| {
        request_error(
            request,
            "webhook_dispatch_failed",
            format!("Failed to serialize lifecycle webhook payload: {error}"),
        )
    })?;

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Some(auth) = &webhook.auth {
        let secret_ref = auth.secret_ref.as_ref().ok_or_else(|| {
            request_error(
                request,
                "webhook_secret_missing",
                format!(
                    "Lifecycle item '{}' requires a local secret_ref auth source, but none was configured.",
                    hook.key
                ),
            )
        })?;
        let secret = env::var(secret_ref).map_err(|_| {
            request_error(
                request,
                "webhook_secret_missing",
                format!(
                    "Lifecycle item '{}' requires secret '{}', but it was not available in this execution context.",
                    hook.key, secret_ref
                ),
            )
        })?;
        apply_lifecycle_auth_headers(&mut headers, auth.scheme, &secret, &body)?;
    }

    Ok((
        rewrite_canonical_yaffle_host_url(&webhook.url),
        body,
        headers,
    ))
}

fn lifecycle_dispatch_requires_control_plane(hook: &LifecycleHook) -> bool {
    match &hook.dispatch {
        LifecycleHookDispatch::GitHubRepositoryDispatch(_) => true,
        LifecycleHookDispatch::Generic(request) => request
            .auth
            .as_ref()
            .and_then(|auth| auth.connection.as_ref())
            .is_some(),
    }
}

fn lifecycle_control_plane_dispatch_body(
    hook: &LifecycleHook,
    payload: Value,
    run_id: &str,
    item_id: &str,
    environment_name: &str,
    workspace_path: &str,
    phase: &str,
) -> Result<Value, EngineError> {
    let dispatch = match &hook.dispatch {
        LifecycleHookDispatch::Generic(request) => {
            let auth = request.auth.as_ref().map(|auth| {
                json!({
                    "scheme": lifecycle_auth_scheme_name(auth.scheme),
                    "connection": auth.connection,
                })
            });
            json!({
                "kind": "generic",
                "request": {
                    "url": request.url,
                    "method": request.method,
                    "auth": auth,
                },
            })
        }
        LifecycleHookDispatch::GitHubRepositoryDispatch(github) => json!({
            "kind": "github_repository_dispatch",
            "github": {
                "owner": github.owner,
                "repo": github.repo,
                "eventType": github.event_type,
                "apiUrl": github.api_url,
            },
        }),
    };

    Ok(json!({
        "runId": run_id,
        "itemId": item_id,
        "environmentName": environment_name,
        "workspacePath": workspace_path,
        "phase": phase,
        "dispatch": dispatch,
        "payload": payload,
    }))
}

fn lifecycle_dispatch_payload(
    context: &LifecycleDispatchContext,
    repo_root: &Path,
    workspace_path: &str,
    environment_name: &str,
    phase: &str,
    hook_key: &str,
    outputs_json: &serde_json::Map<String, serde_json::Value>,
    on_completion_url: Option<String>,
) -> Value {
    let mut payload = serde_json::Map::from_iter([
        (
            "repo_namespace".to_string(),
            json!(context.canonical_repo_namespace),
        ),
        ("environment".to_string(), json!(environment_name)),
        ("workspace_path".to_string(), json!(workspace_path)),
        ("item_key".to_string(), json!(hook_key)),
        ("phase".to_string(), json!(phase)),
        ("outputs".to_string(), Value::Object(outputs_json.clone())),
        ("on_completion".to_string(), json!(on_completion_url)),
    ]);

    if let Some(git_sha) = current_git_sha(repo_root) {
        payload.insert("git_sha".to_string(), json!(git_sha));
    }
    if let Some(git_branch) = current_git_branch(repo_root) {
        payload.insert("git_branch".to_string(), json!(git_branch));
    }

    Value::Object(payload)
}

fn lifecycle_destination_url(
    hook: &LifecycleHook,
    canonical_repo_namespace: &str,
) -> Result<String, EngineError> {
    match &hook.dispatch {
        LifecycleHookDispatch::Generic(request) => Ok(request.url.clone()),
        LifecycleHookDispatch::GitHubRepositoryDispatch(github) => {
            github_repository_dispatch_url(github, Some(canonical_repo_namespace))
        }
    }
}

fn lifecycle_destination_class(
    hook: &LifecycleHook,
    canonical_repo_namespace: &str,
) -> Result<String, EngineError> {
    Ok(
        classify_destination_url(&lifecycle_destination_url(hook, canonical_repo_namespace)?)
            .to_string(),
    )
}

fn github_repository_dispatch_url(
    github: &LifecycleGitHubRepositoryDispatch,
    canonical_repo_namespace: Option<&str>,
) -> Result<String, EngineError> {
    let (owner, repo) = match (&github.owner, &github.repo) {
        (Some(owner), Some(repo)) => (owner.clone(), repo.clone()),
        (None, None) => owner_repo_from_namespace(canonical_repo_namespace.ok_or_else(|| {
            request_error_without_context(
                "repo_namespace_unresolved",
                "GitHub repository_dispatch hooks require a canonical repo namespace or explicit github.owner/github.repo settings.",
            )
        })?)?,
        _ => {
            return Err(request_error_without_context(
                "lifecycle_dispatch_invalid",
                "GitHub repository_dispatch hooks must set both github.owner and github.repo together.",
            ))
        }
    };
    let api_base = github
        .api_url
        .as_deref()
        .unwrap_or("https://api.github.com")
        .trim_end_matches('/');

    Ok(format!("{api_base}/repos/{owner}/{repo}/dispatches"))
}

fn apply_lifecycle_auth_headers(
    headers: &mut HeaderMap,
    scheme: LifecycleWebhookAuthScheme,
    secret: &str,
    body: &[u8],
) -> Result<(), EngineError> {
    match scheme {
        LifecycleWebhookAuthScheme::Bearer => {
            let value = HeaderValue::from_str(&format!("Bearer {secret}")).map_err(|error| {
                request_error_without_context("webhook_header_invalid", error.to_string())
            })?;
            headers.insert(AUTHORIZATION, value);
        }
        LifecycleWebhookAuthScheme::HmacSha256 => {
            type HmacSha256 = Hmac<Sha256>;
            let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).map_err(|error| {
                request_error_without_context("webhook_header_invalid", error.to_string())
            })?;
            mac.update(body);
            let signature = format!("sha256={}", hex_encode(&mac.finalize().into_bytes()));
            headers.insert(
                "X-Yaffle-Signature",
                HeaderValue::from_str(&signature).map_err(|error| {
                    request_error_without_context("webhook_header_invalid", error.to_string())
                })?,
            );
        }
    }

    Ok(())
}

fn lifecycle_auth_scheme_name(scheme: LifecycleWebhookAuthScheme) -> &'static str {
    match scheme {
        LifecycleWebhookAuthScheme::Bearer => "bearer",
        LifecycleWebhookAuthScheme::HmacSha256 => "hmac_sha256",
    }
}

fn classify_destination_url(url: &str) -> &'static str {
    match reqwest::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(|host| host.to_string()))
    {
        Some(host) if is_private_destination_host(&host) => "private_local",
        _ => "public",
    }
}

fn is_private_destination_host(host: &str) -> bool {
    host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
        || host.ends_with(".local")
        || host.starts_with("10.")
        || host.starts_with("192.168.")
        || host.starts_with("172.16.")
}

fn wait_for_lifecycle_item_settlement(
    principal: &StoredPrincipalCredential,
    item_id: &str,
    timeout_hint: Option<&str>,
) -> Result<crate::local_first::LifecycleItemSnapshot, LocalFirstError> {
    let timeout = timeout_hint
        .map(parse_duration_hint)
        .transpose()
        .map_err(|error| LocalFirstError::Config(error.error.message))?
        .unwrap_or_else(|| Duration::from_secs(300));
    let deadline = Instant::now() + timeout;

    loop {
        let item = get_lifecycle_item(principal, item_id)?;
        match item.state.as_str() {
            "pending" | "running" => {
                if Instant::now() >= deadline {
                    return Err(LocalFirstError::Api(format!(
                        "activation item '{}' timed out waiting for completion",
                        item.key
                    )));
                }
                thread::sleep(Duration::from_secs(1));
            }
            _ => return Ok(item),
        }
    }
}

fn parse_duration_hint(value: &str) -> Result<Duration, EngineError> {
    let trimmed = value.trim();
    let (number, unit) = trimmed
        .chars()
        .partition::<String, _>(|character| character.is_ascii_digit());
    let amount = number.parse::<u64>().map_err(|_| {
        request_error_without_context(
            "duration_invalid",
            format!(
                "Invalid duration '{}': expected digits followed by s, m, or h",
                value
            ),
        )
    })?;

    match unit.as_str() {
        "s" => Ok(Duration::from_secs(amount)),
        "m" => Ok(Duration::from_secs(amount * 60)),
        "h" => Ok(Duration::from_secs(amount * 60 * 60)),
        _ => Err(request_error_without_context(
            "duration_invalid",
            format!("Invalid duration '{}': expected suffix s, m, or h", value),
        )),
    }
}

fn report_lifecycle_failure(callback_url: &str, reason: String) -> Result<(), LocalFirstError> {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?
        .post(callback_url)
        .json(&serde_json::json!({
            "status": "failed",
            "reason": reason,
        }))
        .send()
        .map_err(|error| LocalFirstError::Http(error.to_string()))?;
    Ok(())
}

fn try_load_lifecycle_state(
    repo_context: &RepoContext,
    environment_name: &str,
    diagnostics: &mut Vec<DiagnosticMessage>,
) -> Option<crate::local_first::LifecycleStateSnapshot> {
    let canonical_repo_namespace = repo_context.current_namespace.as_deref()?;

    let principal = match ensure_anonymous_principal() {
        Ok(principal) => principal,
        Err(error) => {
            diagnostics.push(DiagnosticMessage {
                level: DiagnosticLevel::Warning,
                code: Some("lifecycle_state_unavailable".to_string()),
                message: error.friendly_message(),
                workspace_path: None,
                item_key: None,
                details: None,
            });
            return None;
        }
    };
    let local_repo_fingerprint = match compute_local_repo_fingerprint(&repo_context.repo_root) {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            diagnostics.push(DiagnosticMessage {
                level: DiagnosticLevel::Warning,
                code: Some("lifecycle_state_unavailable".to_string()),
                message: error.friendly_message(),
                workspace_path: None,
                item_key: None,
                details: None,
            });
            return None;
        }
    };

    match get_lifecycle_state(
        &principal,
        canonical_repo_namespace,
        &local_repo_fingerprint,
        environment_name,
    ) {
        Ok(state) => state,
        Err(error) => {
            diagnostics.push(DiagnosticMessage {
                level: DiagnosticLevel::Warning,
                code: Some("lifecycle_state_unavailable".to_string()),
                message: error.friendly_message(),
                workspace_path: None,
                item_key: None,
                details: None,
            });
            None
        }
    }
}

fn build_workspace_lifecycle_json(
    workspace_path: &str,
    lifecycle_state: Option<&crate::local_first::LifecycleStateSnapshot>,
) -> Option<Value> {
    let lifecycle_state = lifecycle_state?;
    let items = lifecycle_state
        .items
        .iter()
        .filter(|item| item.workspace_path == workspace_path)
        .cloned()
        .collect::<Vec<_>>();
    if items.is_empty() {
        return None;
    }

    let activation = phase_vector_json(&items, "activation");
    let verification = phase_vector_json(&items, "verification");
    Some(json!({
        "workspace_path": workspace_path,
        "items": items,
        "vector": {
            "activation": activation,
            "verification": verification,
        }
    }))
}

fn build_environment_lifecycle_json(
    lifecycle_state: Option<&crate::local_first::LifecycleStateSnapshot>,
) -> Option<Value> {
    let lifecycle_state = lifecycle_state?;
    Some(json!({
        "run": lifecycle_state.run,
        "items": lifecycle_state.items,
        "vector": {
            "activation": phase_vector_json(&lifecycle_state.items, "activation"),
            "verification": phase_vector_json(&lifecycle_state.items, "verification"),
        }
    }))
}

fn build_environment_conditions(
    result_kind: OperationResultKind,
    materialization: Option<&str>,
    lifecycle_state: Option<&crate::local_first::LifecycleStateSnapshot>,
) -> Vec<Value> {
    let activation_items = lifecycle_state
        .map(|state| {
            state
                .items
                .iter()
                .filter(|item| item.phase == "activation")
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let verification_items = lifecycle_state
        .map(|state| {
            state
                .items
                .iter()
                .filter(|item| item.phase == "verification")
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let infra_ready = materialization == Some("present")
        && matches!(
            result_kind,
            OperationResultKind::Succeeded | OperationResultKind::Degraded
        );
    let activation_settled = lifecycle_settled(&activation_items);
    let verification_settled = lifecycle_settled(&verification_items);
    let usable = infra_ready && scoped_condition_met(&activation_items, "usable", true);
    let acceptable = infra_ready
        && scoped_condition_met(&activation_items, "acceptable", false)
        && scoped_condition_met(&verification_items, "acceptable", false);
    let teardown_settled = materialization == Some("absent");

    vec![
        condition_json(
            "infra_ready",
            infra_ready,
            materialization_reason(materialization),
        ),
        condition_json(
            "activation_settled",
            activation_settled,
            if activation_settled {
                None
            } else {
                Some("activation items are still pending or running")
            },
        ),
        condition_json(
            "verification_settled",
            verification_settled,
            if verification_settled {
                None
            } else {
                Some("verification items are still pending or running")
            },
        ),
        condition_json(
            "usable",
            usable,
            if usable {
                None
            } else {
                Some("activation items are still blocking environment usability")
            },
        ),
        condition_json(
            "acceptable",
            acceptable,
            if acceptable {
                None
            } else {
                Some("activation or verification items have not reached an acceptable state")
            },
        ),
        condition_json(
            "teardown_settled",
            teardown_settled,
            if teardown_settled {
                None
            } else {
                Some("environment still has materialized resources")
            },
        ),
    ]
}

fn phase_vector_json(items: &[crate::local_first::LifecycleItemSnapshot], phase: &str) -> Value {
    let mut vector = LifecyclePhaseVector::default();
    for item in items.iter().filter(|item| item.phase == phase) {
        increment_phase_vector(&mut vector, &item.state);
    }
    json!({
        "pending": vector.pending,
        "running": vector.running,
        "succeeded": vector.succeeded,
        "degraded": vector.degraded,
        "blocked": vector.blocked,
        "failed": vector.failed,
    })
}

fn increment_phase_vector(vector: &mut LifecyclePhaseVector, state: &str) {
    match state {
        "pending" => vector.pending += 1,
        "running" => vector.running += 1,
        "succeeded" => vector.succeeded += 1,
        "degraded" => vector.degraded += 1,
        "blocked" => vector.blocked += 1,
        "failed" => vector.failed += 1,
        _ => {}
    }
}

fn lifecycle_settled(items: &[crate::local_first::LifecycleItemSnapshot]) -> bool {
    !items
        .iter()
        .any(|item| matches!(item.state.as_str(), "pending" | "running"))
}

fn scoped_condition_met(
    items: &[crate::local_first::LifecycleItemSnapshot],
    scope: &str,
    allow_degraded: bool,
) -> bool {
    !items.iter().any(|item| {
        let in_scope = item.scopes.iter().any(|value| value == scope);
        in_scope
            && (matches!(
                item.state.as_str(),
                "pending" | "running" | "blocked" | "failed"
            ) || (!allow_degraded && item.state == "degraded"))
    })
}

fn condition_json(name: &str, met: bool, reason: Option<&str>) -> Value {
    json!({
        "name": name,
        "met": met,
        "summary": if met { "succeeded" } else { "blocked" },
        "reason": reason,
    })
}

fn materialization_reason(materialization: Option<&str>) -> Option<&'static str> {
    match materialization {
        Some("present") => None,
        Some("absent") => Some("environment has not been materialized"),
        Some("partially_present") => Some("environment is only partially materialized"),
        Some("materializing") => Some("environment is still materializing"),
        Some("dematerializing") => Some("environment is being torn down"),
        Some("residual") => Some("environment has residual resources"),
        _ => Some("environment materialization is unknown"),
    }
}

fn lifecycle_failure_policy_name(policy: LifecycleFailurePolicy) -> &'static str {
    match policy {
        LifecycleFailurePolicy::Failed => "failed",
        LifecycleFailurePolicy::Degraded => "degraded",
    }
}

fn title_case_phase(phase: &str) -> &'static str {
    match phase {
        "activation" => "Activation",
        "verification" => "Verification",
        _ => "Lifecycle",
    }
}

fn request_error_without_context(
    code: impl Into<String>,
    message: impl Into<String>,
) -> EngineError {
    EngineError {
        contract_version: CONTRACT_VERSION,
        operation: None,
        target: None,
        selection: Some(WorkspaceSelection::default()),
        error: yaffle_contracts::ErrorPayload {
            code: code.into(),
            message: message.into(),
            details: None,
        },
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn terraform_outputs_json(
    request: &EngineRequest,
    outputs: &BTreeMap<String, TerraformOutput>,
) -> Result<serde_json::Map<String, serde_json::Value>, EngineError> {
    let value = serde_json::to_value(outputs).map_err(|error| {
        request_error(
            request,
            "outputs_serialize_failed",
            format!("Failed to serialize outputs for publication: {error}"),
        )
    })?;

    value.as_object().cloned().ok_or_else(|| {
        request_error(
            request,
            "outputs_serialize_failed",
            "serialized outputs were not an object",
        )
    })
}

fn format_converge_summary(environment_name: &str, workspace_paths: &[String]) -> String {
    let header = format!(
        "converged {} workspace(s) for environment '{}'",
        workspace_paths.len(),
        environment_name
    );

    if workspace_paths.is_empty() {
        return header;
    }

    let mut lines = vec![header, String::new()];
    lines.extend(
        workspace_paths
            .iter()
            .map(|workspace_path| format!("- {workspace_path}")),
    );
    lines.join("\n")
}

fn write_workspace_variables_file(
    request: &EngineRequest,
    workspace_dir: &Path,
    workspace: &yaffle_config::Workspace,
    environment_name: &str,
    environment_kind: EnvironmentKind,
) -> Result<(), EngineError> {
    let mut variables = workspace_variable_values(&workspace.variables);
    if let Some(module_api_host_override) = module_api_host_override() {
        variables.insert(
            "module_registry_host".to_string(),
            json!(module_api_host_override),
        );
    }
    variables.insert("environment".to_string(), json!(environment_name));
    variables.insert(
        "environment_kind".to_string(),
        json!(environment_kind_name(environment_kind)),
    );

    let serialized = serde_json::to_vec_pretty(&variables).map_err(|error| {
        request_error(
            request,
            "workspace_variables_serialize_failed",
            format!(
                "Failed to serialize variables for workspace '{}': {error}",
                workspace.path
            ),
        )
    })?;

    fs::write(workspace_dir.join("yaffle.auto.tfvars.json"), serialized).map_err(|error| {
        request_error(
            request,
            "workspace_variables_write_failed",
            format!(
                "Failed to write Yaffle variables file for workspace '{}': {error}",
                workspace.path
            ),
        )
    })
}

fn ensure_injected_variable_declarations(
    request: &EngineRequest,
    workspace_dir: &Path,
) -> Result<(), EngineError> {
    let body = load_workspace_hcl_body(request, workspace_dir)?;
    let declared_variables = body
        .blocks()
        .filter(|block| block.identifier() == "variable")
        .filter_map(|block| {
            block
                .labels()
                .first()
                .map(|label| label.as_str().to_string())
        })
        .collect::<BTreeSet<_>>();

    let missing_variables = [
        ("environment", "Environment name (injected by Yaffle)"),
        (
            "environment_kind",
            "Environment kind: 'named' or 'transient' (injected by Yaffle)",
        ),
    ]
    .into_iter()
    .filter(|(name, _)| !declared_variables.contains(*name))
    .collect::<Vec<_>>();

    if missing_variables.is_empty() {
        return Ok(());
    }

    let blocks = missing_variables
        .into_iter()
        .map(|(name, description)| {
            format!(
                "variable \"{name}\" {{\n  type        = string\n  description = \"{description}\"\n}}"
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let content = format!(
        "# Generated by Yaffle -- do not edit\n# These variables are injected by Yaffle if not declared in the workspace.\n\n{blocks}\n"
    );

    fs::write(workspace_dir.join("yaffle_injected_variables.tf"), content).map_err(|error| {
        request_error(
            request,
            "workspace_variables_write_failed",
            format!(
                "Failed to write injected variable declarations in '{}': {error}",
                workspace_dir.display()
            ),
        )
    })
}

fn workspace_has_explicit_backend(
    request: &EngineRequest,
    workspace_dir: &Path,
) -> Result<bool, EngineError> {
    let body = load_workspace_hcl_body(request, workspace_dir)?;

    Ok(body
        .blocks()
        .filter(|block| block.identifier() == "terraform")
        .any(|block| {
            block
                .body()
                .blocks()
                .any(|nested| nested.identifier() == "cloud" || nested.identifier() == "backend")
        }))
}

fn write_local_backend_override(
    request: &EngineRequest,
    prepared_repo: &PreparedExecutionRepo,
    workspace_dir: &Path,
    workspace_path: &str,
) -> Result<(), EngineError> {
    let environment_name = request
        .target
        .as_ref()
        .map(|target| target.environment.as_str())
        .unwrap_or("unknown");
    let state_path =
        local_backend_state_path(&prepared_repo.repo_root, environment_name, workspace_path);

    if let Some(parent) = state_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            request_error(
                request,
                "workspace_backend_write_failed",
                format!(
                    "Failed to create local state directory for workspace '{}': {error}",
                    workspace_path
                ),
            )
        })?;
    }

    let content = format!(
        "# Generated by Yaffle -- do not edit\nterraform {{\n  backend \"local\" {{\n    path = \"{}\"\n  }}\n}}\n",
        state_path.display()
    );

    fs::write(workspace_dir.join("backend_override.tf"), content).map_err(|error| {
        request_error(
            request,
            "workspace_backend_write_failed",
            format!(
                "Failed to write local backend override for workspace '{}': {error}",
                workspace_path
            ),
        )
    })
}

fn persist_local_backend_state(
    request: &EngineRequest,
    repo_context: &RepoContext,
    prepared_repo: &PreparedExecutionRepo,
    workspace_path: &str,
) -> Result<(), EngineError> {
    let environment_name = request
        .target
        .as_ref()
        .map(|target| target.environment.as_str())
        .unwrap_or("unknown");
    let source_state_dir =
        local_backend_state_path(&prepared_repo.repo_root, environment_name, workspace_path)
            .parent()
            .expect("local backend state path should have a parent")
            .to_path_buf();
    let destination_state_dir =
        local_backend_state_path(&repo_context.repo_root, environment_name, workspace_path)
            .parent()
            .expect("local backend state path should have a parent")
            .to_path_buf();

    if !source_state_dir.is_dir() {
        return Ok(());
    }

    create_private_state_directory(
        request,
        &repo_context.repo_root,
        &destination_state_dir,
        workspace_path,
    )?;

    for file_name in ["terraform.tfstate", "terraform.tfstate.backup"] {
        let source_path = source_state_dir.join(file_name);
        if !source_path.is_file() {
            continue;
        }

        persist_state_file_atomically(
            request,
            &source_path,
            &destination_state_dir.join(file_name),
            workspace_path,
            file_name,
        )?;
    }

    Ok(())
}

fn create_private_state_directory(
    request: &EngineRequest,
    repo_root: &Path,
    destination: &Path,
    workspace_path: &str,
) -> Result<(), EngineError> {
    let state_root = repo_root.join(".yaffle");
    let relative = destination.strip_prefix(&state_root).map_err(|error| {
        request_error(
            request,
            "workspace_state_persist_failed",
            format!(
                "State directory '{}' is outside the local state root for workspace '{}': {error}",
                destination.display(),
                workspace_path
            ),
        )
    })?;

    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(destination).map_err(|error| {
        request_error(
            request,
            "workspace_state_persist_failed",
            format!(
                "Failed to create persisted state directory for workspace '{}': {error}",
                workspace_path
            ),
        )
    })?;

    #[cfg(unix)]
    {
        let mut directory = state_root;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(|error| {
            request_error(
                request,
                "workspace_state_persist_failed",
                format!(
                    "Failed to secure state directory '{}' for workspace '{}': {error}",
                    directory.display(),
                    workspace_path
                ),
            )
        })?;
        for component in relative.components() {
            directory.push(component);
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(
                |error| {
                    request_error(
                        request,
                        "workspace_state_persist_failed",
                        format!(
                            "Failed to secure state directory '{}' for workspace '{}': {error}",
                            directory.display(),
                            workspace_path
                        ),
                    )
                },
            )?;
        }
    }

    Ok(())
}

fn persist_state_file_atomically(
    request: &EngineRequest,
    source: &Path,
    destination: &Path,
    workspace_path: &str,
    file_name: &str,
) -> Result<(), EngineError> {
    let destination_directory = destination
        .parent()
        .expect("persisted state file should have a parent directory");
    let mut source_file = fs::File::open(source).map_err(|error| {
        request_error(
            request,
            "workspace_state_persist_failed",
            format!(
                "Failed to open local state file '{}' for workspace '{}': {error}",
                file_name, workspace_path
            ),
        )
    })?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(destination_directory).map_err(|error| {
            request_error(
                request,
                "workspace_state_persist_failed",
                format!(
                    "Failed to prepare local state file '{}' for workspace '{}': {error}",
                    file_name, workspace_path
                ),
            )
        })?;

    #[cfg(unix)]
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| {
            request_error(
                request,
                "workspace_state_persist_failed",
                format!(
                    "Failed to secure local state file '{}' for workspace '{}': {error}",
                    file_name, workspace_path
                ),
            )
        })?;

    io::copy(&mut source_file, temporary.as_file_mut()).map_err(|error| {
        request_error(
            request,
            "workspace_state_persist_failed",
            format!(
                "Failed to write local state file '{}' for workspace '{}': {error}",
                file_name, workspace_path
            ),
        )
    })?;
    temporary.as_file().sync_all().map_err(|error| {
        request_error(
            request,
            "workspace_state_persist_failed",
            format!(
                "Failed to sync local state file '{}' for workspace '{}': {error}",
                file_name, workspace_path
            ),
        )
    })?;
    temporary.persist(destination).map_err(|error| {
        request_error(
            request,
            "workspace_state_persist_failed",
            format!(
                "Failed to install local state file '{}' for workspace '{}': {}",
                file_name, workspace_path, error.error
            ),
        )
    })?;

    #[cfg(unix)]
    {
        fs::set_permissions(destination, fs::Permissions::from_mode(0o600)).map_err(|error| {
            request_error(
                request,
                "workspace_state_persist_failed",
                format!(
                    "Failed to secure installed state file '{}' for workspace '{}': {error}",
                    file_name, workspace_path
                ),
            )
        })?;
        fs::File::open(destination_directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                request_error(
                    request,
                    "workspace_state_persist_failed",
                    format!(
                        "Failed to sync state directory for workspace '{}': {error}",
                        workspace_path
                    ),
                )
            })?;
    }

    Ok(())
}

fn settle_command_with_state_persistence<T>(
    command_result: Result<T, EngineError>,
    persistence_result: Result<(), EngineError>,
) -> Result<T, EngineError> {
    match (command_result, persistence_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(command_error), Ok(())) => Err(command_error),
        (Ok(_), Err(persistence_error)) => Err(persistence_error),
        (Err(mut command_error), Err(persistence_error)) => {
            let persistence_message = persistence_error.error.message.clone();
            let persistence_details = json!({
                "code": persistence_error.error.code,
                "message": persistence_message,
                "details": persistence_error.error.details,
            });
            command_error.error.message = format!(
                "{} State persistence also failed: {}",
                command_error.error.message, persistence_message
            );
            command_error
                .error
                .details
                .get_or_insert_with(BTreeMap::new)
                .insert("state_persistence_error".to_string(), persistence_details);
            Err(command_error)
        }
    }
}

fn remove_local_backend_state(
    request: &EngineRequest,
    repo_context: &RepoContext,
    workspace_path: &str,
) -> Result<(), EngineError> {
    let environment_name = request
        .target
        .as_ref()
        .map(|target| target.environment.as_str())
        .unwrap_or("unknown");
    let state_dir =
        local_backend_state_path(&repo_context.repo_root, environment_name, workspace_path)
            .parent()
            .expect("local backend state path should have a parent")
            .to_path_buf();

    if !state_dir.exists() {
        return Ok(());
    }

    fs::remove_dir_all(&state_dir).map_err(|error| {
        request_error(
            request,
            "workspace_state_remove_failed",
            format!(
                "Failed to remove local state for workspace '{}': {error}",
                workspace_path
            ),
        )
    })
}

fn local_backend_state_path(
    repo_root: &Path,
    environment_name: &str,
    workspace_path: &str,
) -> PathBuf {
    repo_root
        .join(".yaffle")
        .join("state")
        .join(environment_name)
        .join(workspace_path)
        .join("terraform.tfstate")
}

fn local_tf_data_dir(repo_root: &Path, environment_name: &str, workspace_path: &str) -> PathBuf {
    repo_root
        .join(".yaffle")
        .join("tf-data")
        .join(environment_name)
        .join(workspace_path)
}

fn terraform_output_type_name(value: Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(value) => Some(value),
        other => serde_json::to_string(&other).ok(),
    }
}

fn workspace_variable_values(
    variables: &BTreeMap<String, yaffle_config::VariableValue>,
) -> BTreeMap<String, Value> {
    variables
        .iter()
        .map(|(name, value)| {
            let value = match value {
                yaffle_config::VariableValue::String(value) => json!(value),
                yaffle_config::VariableValue::Integer(value) => json!(value),
                yaffle_config::VariableValue::Float(value) => json!(value),
                yaffle_config::VariableValue::Boolean(value) => json!(value),
            };

            (name.clone(), value)
        })
        .collect()
}

fn environment_kind_name(environment_kind: EnvironmentKind) -> &'static str {
    match environment_kind {
        EnvironmentKind::Named => "named",
        EnvironmentKind::Transient => "transient",
    }
}

fn load_workspace_hcl_body(
    request: &EngineRequest,
    workspace_dir: &Path,
) -> Result<Body, EngineError> {
    let mut body = Body::default();

    for entry in fs::read_dir(workspace_dir).map_err(|error| {
        request_error(
            request,
            "workspace_read_failed",
            format!(
                "Failed to read workspace directory '{}': {error}",
                workspace_dir.display()
            ),
        )
    })? {
        let entry = entry.map_err(|error| {
            request_error(
                request,
                "workspace_read_failed",
                format!(
                    "Failed to inspect workspace directory '{}': {error}",
                    workspace_dir.display()
                ),
            )
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("tf") {
            continue;
        }

        let content = fs::read_to_string(&path).map_err(|error| {
            request_error(
                request,
                "workspace_read_failed",
                format!(
                    "Failed to read workspace file '{}': {error}",
                    path.display()
                ),
            )
        })?;
        let parsed = hcl::from_str::<Body>(&content).map_err(|error| {
            request_error(
                request,
                "workspace_parse_failed",
                format!(
                    "Failed to parse workspace file '{}': {error}",
                    path.display()
                ),
            )
        })?;
        body.extend(parsed);
    }

    Ok(body)
}

fn copy_repo_for_execution(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;

    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        if should_skip_execution_copy(&name) {
            continue;
        }

        let target = destination.join(&file_name);
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_repo_for_execution(&path, &target)?;
        } else if file_type.is_file() {
            fs::copy(&path, &target)?;
        }
    }

    Ok(())
}

fn should_skip_execution_copy(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".jj" | ".dev" | ".direnv" | "target" | "node_modules" | ".terraform"
    )
}

fn slugify_path(path: &str) -> String {
    path.chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' => character,
            _ => '-',
        })
        .collect()
}

fn utf8_trimmed(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn shell_append_export(name: &str, prefix: &str) -> String {
    format!(
        "export {name}={}\"${{{name}:+ ${{{name}}}}}\"",
        shell_single_quote(prefix)
    )
}

fn emit_progress(
    reporter: &mut Option<&mut dyn EngineProgressReporter>,
    event: EngineProgressEvent,
) {
    if let Some(reporter) = reporter.as_mut() {
        (**reporter).emit(event);
    }
}

fn build_response(
    request: &EngineRequest,
    result_kind: OperationResultKind,
    summary: impl Into<String>,
    workspaces: Vec<WorkspaceSnapshot>,
    outputs: BTreeMap<String, TerraformOutput>,
    diagnostics: Vec<DiagnosticMessage>,
) -> EngineResponse {
    build_response_with_environment(
        request,
        result_kind,
        summary,
        None,
        workspaces,
        outputs,
        diagnostics,
    )
}

fn build_response_with_environment(
    request: &EngineRequest,
    result_kind: OperationResultKind,
    summary: impl Into<String>,
    environment: Option<EnvironmentSnapshot>,
    workspaces: Vec<WorkspaceSnapshot>,
    outputs: BTreeMap<String, TerraformOutput>,
    diagnostics: Vec<DiagnosticMessage>,
) -> EngineResponse {
    EngineResponse {
        contract_version: CONTRACT_VERSION,
        operation: request.operation.clone(),
        target: request.target.clone(),
        selection: request.selection.clone(),
        result: OperationResult {
            kind: result_kind,
            summary: summary.into(),
        },
        environment,
        workspaces,
        outputs,
        workspace_outputs: BTreeMap::new(),
        diagnostics,
        metrics: None,
    }
}

fn doctor_result_kind(error_count: usize, warning_count: usize) -> OperationResultKind {
    if error_count > 0 {
        OperationResultKind::Failed
    } else if warning_count > 0 {
        OperationResultKind::Degraded
    } else {
        OperationResultKind::Succeeded
    }
}

fn format_doctor_summary(
    info_count: usize,
    warning_count: usize,
    error_count: usize,
    summary_lines: &[String],
) -> String {
    let header = if error_count > 0 {
        format!(
            "doctor found {} error(s), {} warning(s), {} passing check(s)",
            error_count, warning_count, info_count
        )
    } else if warning_count > 0 {
        format!(
            "doctor found {} warning(s), {} passing check(s)",
            warning_count, info_count
        )
    } else {
        format!("doctor passed {} check(s)", info_count)
    };

    let mut lines = vec![header, String::new()];
    lines.extend(summary_lines.iter().cloned());
    lines.join("\n")
}

#[allow(clippy::too_many_arguments)]
fn record_doctor_check(
    diagnostics: &mut Vec<DiagnosticMessage>,
    summary_lines: &mut Vec<String>,
    info_count: &mut usize,
    warning_count: &mut usize,
    error_count: &mut usize,
    level: DiagnosticLevel,
    code: impl Into<String>,
    message: impl Into<String>,
    details: Option<BTreeMap<String, Value>>,
) {
    let code = code.into();
    let message = message.into();

    match level {
        DiagnosticLevel::Info => {
            *info_count += 1;
            summary_lines.push(format!("[ok] {message}"));
        }
        DiagnosticLevel::Warning => {
            *warning_count += 1;
            summary_lines.push(format!("[warn] {message}"));
        }
        DiagnosticLevel::Error => {
            *error_count += 1;
            summary_lines.push(format!("[fail] {message}"));
        }
    }

    diagnostics.push(DiagnosticMessage {
        level,
        code: Some(code),
        message,
        workspace_path: None,
        item_key: None,
        details,
    });
}

#[allow(clippy::too_many_arguments)]
fn record_graph_error(
    diagnostics: &mut Vec<DiagnosticMessage>,
    summary_lines: &mut Vec<String>,
    info_count: &mut usize,
    warning_count: &mut usize,
    error_count: &mut usize,
    error: EngineError,
    scope: Option<String>,
) {
    let summary_message = match &scope {
        Some(scope) => format!("{}: {}", scope, error.error.message),
        None => error.error.message.clone(),
    };
    let details = with_optional_scope_details(scope, error.error.details);

    record_doctor_check(
        diagnostics,
        summary_lines,
        info_count,
        warning_count,
        error_count,
        DiagnosticLevel::Error,
        error.error.code,
        summary_message,
        details,
    );
}

fn with_optional_scope_details(
    scope: Option<String>,
    details: Option<BTreeMap<String, Value>>,
) -> Option<BTreeMap<String, Value>> {
    match (scope, details) {
        (None, None) => None,
        (Some(scope), None) => Some(BTreeMap::from([("scope".to_string(), json!(scope))])),
        (None, Some(details)) => Some(details),
        (Some(scope), Some(mut details)) => {
            details.insert("scope".to_string(), json!(scope));
            Some(details)
        }
    }
}

fn graph_edge_count(graph: &ResolvedWorkspaceGraph) -> usize {
    graph
        .workspaces
        .iter()
        .map(|workspace| workspace.dependencies.len())
        .sum()
}

fn tofu_source_label(source: TofuSourceKind) -> &'static str {
    match source {
        TofuSourceKind::Override => "override",
        TofuSourceKind::Bundled => "bundled",
        TofuSourceKind::Managed => "managed",
        TofuSourceKind::System => "system",
    }
}

fn request_error(
    request: &EngineRequest,
    code: impl Into<String>,
    message: impl Into<String>,
) -> EngineError {
    request_error_with_details(request, code, message, None)
}

fn request_error_with_details(
    request: &EngineRequest,
    code: impl Into<String>,
    message: impl Into<String>,
    details: Option<BTreeMap<String, Value>>,
) -> EngineError {
    EngineError {
        contract_version: CONTRACT_VERSION,
        operation: Some(request.operation.clone()),
        target: request.target.clone(),
        selection: Some(request.selection.clone()),
        error: yaffle_contracts::ErrorPayload {
            code: code.into(),
            message: message.into(),
            details,
        },
    }
}

fn local_first_error(
    request: &EngineRequest,
    code: impl Into<String>,
    error: LocalFirstError,
) -> EngineError {
    request_error(request, code, error.friendly_message())
}

fn find_yaffle_toml(start: &Path) -> Option<PathBuf> {
    for directory in start.ancestors() {
        let candidate = directory.join("yaffle.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

fn derive_current_namespace(repo_root: &Path) -> Option<String> {
    let config_path = resolve_git_config_path(repo_root)?;
    let config = fs::read_to_string(config_path).ok()?;
    let remote_url = extract_preferred_remote_url(&config)?;
    namespace_from_remote_url(&remote_url)
}

fn resolve_git_config_path(repo_root: &Path) -> Option<PathBuf> {
    let git_path = repo_root.join(".git");

    if git_path.is_dir() {
        return Some(git_path.join("config"));
    }

    if git_path.is_file() {
        let pointer = fs::read_to_string(git_path).ok()?;
        let git_dir = pointer.strip_prefix("gitdir:")?.trim();
        let git_dir = PathBuf::from(git_dir);
        let resolved = if git_dir.is_absolute() {
            git_dir
        } else {
            repo_root.join(git_dir)
        };

        return Some(resolved.join("config"));
    }

    None
}

fn extract_preferred_remote_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    let mut fallback = None;

    for line in config.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_origin = trimmed == "[remote \"origin\"]";
            continue;
        }

        let Some(url) = trimmed.strip_prefix("url =") else {
            continue;
        };
        let url = url.trim().to_string();

        if in_origin {
            return Some(url);
        }

        if fallback.is_none() {
            fallback = Some(url);
        }
    }

    fallback
}

fn namespace_from_remote_url(remote_url: &str) -> Option<String> {
    for prefix in [
        "git@github.com:",
        "ssh://git@github.com/",
        "https://github.com/",
        "http://github.com/",
    ] {
        if let Some(path) = remote_url.strip_prefix(prefix) {
            return namespace_from_owner_repo_path(path);
        }
    }

    None
}

fn namespace_from_owner_repo_path(path: &str) -> Option<String> {
    let path = path.trim_start_matches('/');
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);

    if owner.is_empty() || repo.is_empty() {
        return None;
    }

    Some(format!("{owner}--{repo}"))
}

fn owner_repo_from_namespace(namespace: &str) -> Result<(String, String), EngineError> {
    let Some((owner, repo)) = namespace.split_once("--") else {
        return Err(request_error_without_context(
            "repo_namespace_unresolved",
            format!(
                "Could not derive owner/repo from canonical namespace '{}'.",
                namespace
            ),
        ));
    };
    if owner.trim().is_empty() || repo.trim().is_empty() {
        return Err(request_error_without_context(
            "repo_namespace_unresolved",
            format!(
                "Canonical namespace '{}' must include both owner and repo.",
                namespace
            ),
        ));
    }

    Ok((owner.to_string(), repo.to_string()))
}

fn current_git_sha(repo_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let sha = String::from_utf8(output.stdout).ok()?;
    let trimmed = sha.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn current_git_branch(repo_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--abbrev-ref")
        .arg("HEAD")
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8(output.stdout).ok()?;
    let trimmed = branch.trim();
    (!trimmed.is_empty() && trimmed != "HEAD").then(|| trimmed.to_string())
}

fn graph_error(request: &EngineRequest, error: GraphError) -> EngineError {
    match error {
        GraphError::UnknownWorkspace { workspace } => request_error_with_details(
            request,
            "invalid_workspace_selection",
            format!(
                "Workspace selection references unknown workspace '{}'.",
                workspace
            ),
            Some(BTreeMap::from([(
                "workspace".to_string(),
                json!(workspace),
            )])),
        ),
        GraphError::WorkspaceNotInEnvironment {
            workspace,
            environment,
        } => request_error_with_details(
            request,
            "workspace_not_in_environment",
            format!(
                "Workspace '{}' is not active in environment '{}'.",
                workspace, environment
            ),
            Some(BTreeMap::from([
                ("workspace".to_string(), json!(workspace)),
                ("environment".to_string(), json!(environment)),
            ])),
        ),
        GraphError::DependencyCycle { cycle_path } => request_error_with_details(
            request,
            "dependency_cycle",
            format!(
                "Workspace dependency cycle detected: {}",
                cycle_path.join(" -> ")
            ),
            Some(BTreeMap::from([(
                "cycle_path".to_string(),
                json!(cycle_path),
            )])),
        ),
    }
}

fn build_dependents(graph: &ResolvedWorkspaceGraph) -> BTreeMap<String, Vec<String>> {
    let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for workspace in &graph.workspaces {
        for dependency in &workspace.dependencies {
            dependents
                .entry(dependency.clone())
                .or_default()
                .push(workspace.path.clone());
        }
    }

    for dependents_for_workspace in dependents.values_mut() {
        dependents_for_workspace.sort();
    }

    dependents
}

fn format_graph_summary(graph: &ResolvedWorkspaceGraph, order: &[String]) -> String {
    let edge_count = graph
        .workspaces
        .iter()
        .map(|workspace| workspace.dependencies.len())
        .sum::<usize>();

    let header = match (&graph.environment, graph.environment_kind) {
        (Some(environment), Some(EnvironmentKind::Named)) => {
            format!("environment-resolved graph for '{}'", environment,)
        }
        (Some(environment), Some(EnvironmentKind::Transient)) => {
            format!("environment-resolved graph for '{}'", environment,)
        }
        _ => "static repo graph".to_string(),
    };

    let dag = render_graph_dag(graph, order);

    [
        header,
        format!("{} workspaces, {} edges", order.len(), edge_count),
        "left to right = dependency depth".to_string(),
        String::new(),
        dag,
    ]
    .join("\n")
}

fn render_graph_dag(graph: &ResolvedWorkspaceGraph, order: &[String]) -> String {
    let layout = build_graph_layout(graph, order);
    let mut canvas = GraphCanvas::new(layout.height(), layout.width());

    let mut edge_id = 0;

    for workspace in &graph.workspaces {
        let target_stage = layout.stage_for(&workspace.path);
        let target_row = layout.row_for(&workspace.path);
        let target_x = layout.x_for_stage(target_stage);

        for dependency in &workspace.dependencies {
            edge_id += 1;
            let source_stage = layout.stage_for(dependency);
            let source_row = layout.row_for(dependency);
            let source_x = layout.x_for_stage(source_stage);

            let source_end = source_x + layout.label_for(dependency).len();
            let bend_x = target_x.saturating_sub(3);
            let arrow_x = target_x.saturating_sub(1);

            if source_row == target_row {
                if source_end <= target_x.saturating_sub(2) {
                    canvas.add_horizontal(source_row, source_end, target_x - 2, edge_id);
                }
            } else {
                if source_end <= bend_x {
                    canvas.add_horizontal(source_row, source_end, bend_x, edge_id);
                }
                canvas.add_vertical(bend_x, source_row, target_row, edge_id);
                canvas.add_horizontal(target_row, bend_x, target_x - 2, edge_id);
            }

            canvas.set_arrow_right(target_row, arrow_x);
        }
    }

    let cells = canvas.into_cells();
    let mut grid = cells
        .iter()
        .map(|row| {
            row.iter()
                .cloned()
                .map(render_graph_cell)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    for (row_index, row) in cells.iter().enumerate() {
        for (column_index, cell) in row.iter().enumerate() {
            if !cell.is_crossing() {
                continue;
            }

            if row_index == 0 || column_index == 0 || column_index + 1 >= grid[row_index].len() {
                continue;
            }

            grid[row_index - 1][column_index - 1] = '╭';
            grid[row_index - 1][column_index] = '─';
            grid[row_index - 1][column_index + 1] = '╮';

            grid[row_index][column_index - 1] = '╯';
            grid[row_index][column_index] = '│';
            grid[row_index][column_index + 1] = '╰';
        }
    }

    for workspace_path in &layout.ordered_nodes {
        let token = layout.label_for(workspace_path);
        let row = layout.row_for(workspace_path);
        let x = layout.x_for_stage(layout.stage_for(workspace_path));
        for (offset, character) in token.chars().enumerate() {
            grid[row][x + offset] = character;
        }
    }

    let title = match (&graph.environment, graph.environment_kind) {
        (Some(environment), Some(EnvironmentKind::Named)) => format!("{} [named]", environment),
        (Some(environment), Some(EnvironmentKind::Transient)) => {
            format!("{} [transient]", environment)
        }
        _ => "repo".to_string(),
    };

    let mut lines = vec![title];
    lines.extend(
        grid.into_iter()
            .map(|row| row.into_iter().collect::<String>())
            .map(|row| row.trim_end().to_string())
            .filter(|row| !row.is_empty()),
    );

    lines.join("\n")
}

#[derive(Debug)]
struct GraphLayout {
    node_labels: BTreeMap<String, String>,
    stage_offsets: Vec<usize>,
    stage_widths: Vec<usize>,
    node_stages: BTreeMap<String, usize>,
    node_rows: BTreeMap<String, usize>,
    ordered_nodes: Vec<String>,
}

impl GraphLayout {
    fn stage_for(&self, workspace_path: &str) -> usize {
        self.node_stages
            .get(workspace_path)
            .copied()
            .expect("workspace should have a stage")
    }

    fn row_for(&self, workspace_path: &str) -> usize {
        self.node_rows
            .get(workspace_path)
            .copied()
            .expect("workspace should have a row")
    }

    fn x_for_stage(&self, stage: usize) -> usize {
        self.stage_offsets[stage]
    }

    fn label_for(&self, workspace_path: &str) -> &str {
        self.node_labels
            .get(workspace_path)
            .map(|label| label.as_str())
            .expect("workspace should have a label")
    }

    fn width(&self) -> usize {
        let max_stage = self.node_stages.values().copied().max().unwrap_or(0);
        self.x_for_stage(max_stage) + self.stage_widths[max_stage]
    }

    fn height(&self) -> usize {
        self.ordered_nodes
            .len()
            .saturating_mul(2)
            .saturating_sub(1)
            .max(1)
    }
}

fn build_graph_layout(graph: &ResolvedWorkspaceGraph, order: &[String]) -> GraphLayout {
    let topo_rank = order
        .iter()
        .enumerate()
        .map(|(index, workspace_path)| (workspace_path.clone(), index))
        .collect::<BTreeMap<_, _>>();

    let mut node_stages = BTreeMap::new();
    let node_labels = order
        .iter()
        .map(|workspace_path| (workspace_path.clone(), format!("[{workspace_path}]")))
        .collect::<BTreeMap<_, _>>();

    for workspace_path in order {
        let workspace = graph
            .workspace(workspace_path)
            .expect("ordered workspace should exist in resolved graph");
        let stage = workspace
            .dependencies
            .iter()
            .map(|dependency| node_stages.get(dependency).copied().unwrap_or(0) + 1)
            .max()
            .unwrap_or(0);

        node_stages.insert(workspace.path.clone(), stage);
    }

    let max_stage = node_stages.values().copied().max().unwrap_or(0);
    let mut stage_widths = vec![0; max_stage + 1];
    for workspace_path in order {
        let stage = node_stages.get(workspace_path).copied().unwrap_or(0);
        stage_widths[stage] = stage_widths[stage].max(node_labels[workspace_path].len());
    }

    let mut stage_offsets = Vec::with_capacity(stage_widths.len());
    let mut current_offset = 0;
    for stage_width in &stage_widths {
        stage_offsets.push(current_offset);
        current_offset += *stage_width + 6;
    }

    let mut ordered_nodes = order.to_vec();
    ordered_nodes.sort_by_key(|workspace_path| {
        (
            node_stages.get(workspace_path).copied().unwrap_or(0),
            topo_rank.get(workspace_path).copied().unwrap_or(usize::MAX),
        )
    });

    let node_rows = ordered_nodes
        .iter()
        .enumerate()
        .map(|(index, workspace_path)| (workspace_path.clone(), index * 2))
        .collect();

    GraphLayout {
        node_labels,
        stage_offsets,
        stage_widths,
        node_stages,
        node_rows,
        ordered_nodes,
    }
}

#[derive(Clone, Debug, Default)]
struct GraphCell {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
    arrow_right: bool,
    horizontal_edges: BTreeSet<usize>,
    vertical_edges: BTreeSet<usize>,
}

#[derive(Debug)]
struct GraphCanvas {
    cells: Vec<Vec<GraphCell>>,
}

impl GraphCanvas {
    fn new(height: usize, width: usize) -> Self {
        Self {
            cells: vec![vec![GraphCell::default(); width]; height],
        }
    }

    fn add_horizontal(&mut self, row: usize, start_x: usize, end_x: usize, edge_id: usize) {
        if start_x >= self.cells[row].len() || end_x >= self.cells[row].len() || start_x > end_x {
            return;
        }

        for x in start_x..=end_x {
            self.cells[row][x].horizontal_edges.insert(edge_id);
            if x > start_x {
                self.cells[row][x].left = true;
            }
            if x < end_x {
                self.cells[row][x].right = true;
            }
        }
    }

    fn add_vertical(&mut self, column: usize, start_row: usize, end_row: usize, edge_id: usize) {
        if start_row == end_row || column >= self.cells[0].len() {
            return;
        }

        let (min_row, max_row) = if start_row < end_row {
            (start_row, end_row)
        } else {
            (end_row, start_row)
        };

        for row in min_row..=max_row {
            self.cells[row][column].vertical_edges.insert(edge_id);
            if row > min_row {
                self.cells[row][column].up = true;
            }
            if row < max_row {
                self.cells[row][column].down = true;
            }
        }
    }

    fn set_arrow_right(&mut self, row: usize, column: usize) {
        if row >= self.cells.len() || column >= self.cells[row].len() {
            return;
        }

        self.cells[row][column].arrow_right = true;
    }

    fn into_cells(self) -> Vec<Vec<GraphCell>> {
        self.cells
    }
}

impl GraphCell {
    fn is_crossing(&self) -> bool {
        !self.horizontal_edges.is_empty()
            && !self.vertical_edges.is_empty()
            && self.horizontal_edges.is_disjoint(&self.vertical_edges)
    }
}

fn render_graph_cell(cell: GraphCell) -> char {
    if cell.arrow_right {
        return '▶';
    }

    match (cell.up, cell.down, cell.left, cell.right) {
        (false, false, false, false) => ' ',
        (false, false, true, true) => '─',
        (true, true, false, false) => '│',
        (false, true, false, true) => '┌',
        (true, false, false, true) => '└',
        (false, true, true, false) => '┐',
        (true, false, true, false) => '┘',
        (true, true, false, true) => '├',
        (true, true, true, false) => '┤',
        (false, true, true, true) => '┬',
        (true, false, true, true) => '┴',
        (true, true, true, true) => '┼',
        (false, false, false, true) | (false, false, true, false) => '─',
        (false, true, false, false) | (true, false, false, false) => '│',
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{LazyLock, Mutex};
    use std::thread;

    use tempfile::TempDir;
    use yaffle_contracts::{EngineOperation, OperationResultKind};
    use yaffle_tofu::TOFU_OVERRIDE_ENV_VAR;

    use super::*;

    static TOFU_OVERRIDE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    #[test]
    fn command_error_remains_primary_when_state_persistence_also_fails() {
        let request = EngineRequest {
            operation: EngineOperation::Converge,
            target: None,
            selection: WorkspaceSelection::default(),
            wait_for: None,
        };
        let command_error = request_error(&request, "tofu_apply_failed", "apply failed");
        let persistence_error = request_error(
            &request,
            "workspace_state_persist_failed",
            "state copy failed",
        );

        let error =
            settle_command_with_state_persistence::<()>(Err(command_error), Err(persistence_error))
                .expect_err("the command error should remain primary");

        assert_eq!(error.error.code, "tofu_apply_failed");
        assert!(error.error.message.contains("apply failed"));
        assert!(error.error.message.contains("state copy failed"));
        assert_eq!(
            error
                .error
                .details
                .as_ref()
                .and_then(|details| { details["state_persistence_error"]["code"].as_str() }),
            Some("workspace_state_persist_failed")
        );
    }

    #[test]
    fn selects_configured_non_sensitive_outputs_for_egress() {
        let config = parse_yaffle_toml(
            r#"version = 1

[[workspaces]]
path = "infra"
environments = "*"
outputs.endpoint = { visibility = "internal" }
outputs.password = { visibility = "internal" }
"#,
        )
        .expect("config should parse");
        let workspace = &config.workspaces[0];
        let request = EngineRequest {
            operation: EngineOperation::Outputs,
            target: None,
            selection: WorkspaceSelection::default(),
            wait_for: None,
        };
        let outputs = BTreeMap::from([
            (
                "endpoint".to_string(),
                TerraformOutput {
                    value: json!("https://api.example.test"),
                    type_name: Some("string".to_string()),
                    sensitive: Some(false),
                },
            ),
            (
                "unselected".to_string(),
                TerraformOutput {
                    value: json!("private"),
                    type_name: Some("string".to_string()),
                    sensitive: Some(false),
                },
            ),
        ]);

        let selected = select_workspace_outputs(&request, workspace, &outputs)
            .expect("non-sensitive selected output should pass");
        assert_eq!(
            selected.keys().cloned().collect::<Vec<_>>(),
            vec!["endpoint"]
        );

        let sensitive_outputs = BTreeMap::from([(
            "password".to_string(),
            TerraformOutput {
                value: json!("do-not-publish"),
                type_name: Some("string".to_string()),
                sensitive: Some(true),
            },
        )]);
        let error = select_workspace_outputs(&request, workspace, &sensitive_outputs)
            .expect_err("sensitive selected output should fail closed");
        assert!(error.error.message.contains("password"));
        assert!(!error.error.message.contains("do-not-publish"));

        let redacted = redact_sensitive_outputs(&request, &sensitive_outputs)
            .expect("viewer output should be structurally redacted");
        assert_eq!(redacted["password"].value, Value::Null);
        assert_eq!(redacted["password"].sensitive, Some(true));

        let invalid_outputs = BTreeMap::from([(
            "password".to_string(),
            TerraformOutput {
                value: json!("do-not-publish"),
                type_name: Some("string".to_string()),
                sensitive: None,
            },
        )]);
        let error = select_workspace_outputs(&request, workspace, &invalid_outputs)
            .expect_err("missing sensitivity metadata should fail closed");
        assert!(error.error.message.contains("password"));
        assert!(!error.error.message.contains("do-not-publish"));
    }

    #[test]
    fn executes_graph_from_nested_workspace_directory() {
        let repo = TempDir::new().expect("temp dir should exist");

        fs::create_dir_all(repo.path().join(".git")).expect("git dir should exist");
        fs::write(
            repo.path().join(".git/config"),
            "[remote \"origin\"]\n  url = https://github.com/acme/platform.git\n",
        )
        .expect("git config should exist");

        fs::write(
            repo.path().join("yaffle.toml"),
            r#"version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "infra/shared"
environments = ["main"]

[[workspaces]]
path = "apps/web/infra"
environments = ["main"]
"#,
        )
        .expect("config should be written");

        write_workspace_file(
            repo.path(),
            "infra/shared/main.tf",
            "resource \"null_resource\" \"x\" {}\n",
        );
        write_workspace_file(
            repo.path(),
            "apps/web/infra/main.tf",
            r#"variable "module_registry_host" {
  default = "yaffle.dev"
}

module "shared" {
  source = "${var.module_registry_host}/acme--platform/infra--shared/yaffle"
}
"#,
        );

        let response = execute(
            &EngineRequest {
                operation: EngineOperation::Graph,
                target: Some(EnvironmentTarget {
                    environment: "main".to_string(),
                }),
                selection: WorkspaceSelection::default(),
                wait_for: None,
            },
            &repo.path().join("apps/web/infra"),
        )
        .expect("graph should execute");

        assert_eq!(response.result.kind, OperationResultKind::Succeeded);
        assert_eq!(
            response
                .workspaces
                .iter()
                .map(|workspace| workspace.workspace_path.as_str())
                .collect::<Vec<_>>(),
            vec!["infra/shared", "apps/web/infra"],
        );
        assert!(response
            .result
            .summary
            .contains("environment-resolved graph for 'main'"));
        assert!(response
            .result
            .summary
            .contains("left to right = dependency depth"));
        assert!(response.result.summary.contains("main [named]"));
        assert!(response.result.summary.contains("[infra/shared]"));
        assert!(response.result.summary.contains("[apps/web/infra]"));
        assert!(!response.result.summary.contains("legend"));
    }

    #[test]
    fn renders_inline_workspace_names_in_dag() {
        let graph = ResolvedWorkspaceGraph {
            environment: Some("main".to_string()),
            environment_kind: Some(EnvironmentKind::Named),
            workspaces: vec![
                yaffle_graph::WorkspaceNode {
                    path: "infra/shared".to_string(),
                    dependencies: Vec::new(),
                },
                yaffle_graph::WorkspaceNode {
                    path: "infra/production".to_string(),
                    dependencies: Vec::new(),
                },
                yaffle_graph::WorkspaceNode {
                    path: "apps/control-plane/infra".to_string(),
                    dependencies: vec!["infra/shared".to_string(), "infra/production".to_string()],
                },
            ],
        };

        let summary = format_graph_summary(
            &graph,
            &[
                "infra/shared".to_string(),
                "infra/production".to_string(),
                "apps/control-plane/infra".to_string(),
            ],
        );

        assert!(summary.contains("main [named]"));
        assert!(summary.contains("left to right = dependency depth"));
        assert!(summary.contains("[infra/shared]"));
        assert!(summary.contains("[infra/production]"));
        assert!(summary.contains("[apps/control-plane/infra]"));
        assert!(summary.contains("╭─╮") || summary.contains("╯│╰") || summary.contains("▶"));
        assert!(!summary.contains("legend"));
    }

    #[test]
    fn execution_levels_respect_local_remote_state_references() {
        let repo = TempDir::new().expect("temp dir should exist");

        fs::create_dir_all(repo.path().join(".git")).expect("git dir should exist");
        fs::write(
            repo.path().join(".git/config"),
            "[remote \"origin\"]\n  url = https://github.com/test-org/fixture.git\n",
        )
        .expect("git config should exist");

        fs::write(
            repo.path().join("yaffle.toml"),
            r#"version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "infra/shared"
environments = ["main"]

[[workspaces]]
path = "apps/web/infra"
environments = ["main"]
"#,
        )
        .expect("config should be written");

        write_workspace_file(
            repo.path(),
            "infra/shared/main.tf",
            "output \"domain\" { value = \"example.test\" }\n",
        );
        write_workspace_file(
            repo.path(),
            "apps/web/infra/main.tf",
            r#"data "terraform_remote_state" "shared" {
  backend = "local"

  config = {
    path = "../../../.yaffle/state/main/infra/shared/terraform.tfstate"
  }
}
"#,
        );

        let request = EngineRequest {
            operation: EngineOperation::Converge,
            target: Some(EnvironmentTarget {
                environment: "main".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        };
        let repo_context =
            load_repo_context(repo.path(), &request).expect("repo context should load");
        let graph_context = load_graph_context(&repo_context, &request).expect("graph should load");

        let levels = workspace_execution_levels(&request, &repo_context, &graph_context)
            .expect("execution levels should resolve");

        assert_eq!(
            levels,
            vec![
                vec!["infra/shared".to_string()],
                vec!["apps/web/infra".to_string()],
            ]
        );
    }

    #[test]
    fn allows_outputs_without_workspace_selection_in_engine_dispatch() {
        let request = EngineRequest {
            operation: EngineOperation::Outputs,
            target: Some(EnvironmentTarget {
                environment: "main".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        };

        assert!(validate_request(&request).is_ok());
    }

    #[test]
    fn validates_wait_condition_in_engine_dispatch() {
        let error = execute(
            &EngineRequest {
                operation: EngineOperation::Wait,
                target: Some(EnvironmentTarget {
                    environment: "main".to_string(),
                }),
                selection: WorkspaceSelection::default(),
                wait_for: Some("   ".to_string()),
            },
            Path::new("/tmp"),
        )
        .expect_err("wait should require a non-empty condition");

        assert_eq!(error.error.code, "invalid_condition");
    }

    #[test]
    fn discovers_auth_hosts_from_workspace_hcl() {
        let repo = TempDir::new().expect("temp dir should exist");
        let workspace_dir = repo.path().join("infra/app");
        fs::create_dir_all(&workspace_dir).expect("workspace dir should exist");
        fs::write(
            workspace_dir.join("main.tf"),
            r#"terraform {
  cloud {
    hostname = "yaffle.dev"
  }

  backend "remote" {}
}

module "private" {
  source = "registry.internal:8443/test-org/service/aws"
}

module "relative" {
  source = "../modules/shared"
}
"#,
        )
        .expect("fixture file should be written");

        let hosts = discover_workspace_auth_hosts(
            &EngineRequest {
                operation: EngineOperation::Status,
                target: Some(EnvironmentTarget {
                    environment: "main".to_string(),
                }),
                selection: WorkspaceSelection::default(),
                wait_for: None,
            },
            &workspace_dir,
        )
        .expect("auth hosts should be discovered");

        assert_eq!(
            hosts,
            vec![
                "app.terraform.io".to_string(),
                "registry.internal:8443".to_string(),
                "yaffle.dev".to_string(),
            ]
        );
    }

    #[test]
    fn prepares_workspace_auth_from_env_token() {
        let repo = TempDir::new().expect("temp dir should exist");
        let _guard = crate::local_first::LOCAL_FIRST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let workspace_dir = repo.path().join("infra/app");
        fs::create_dir_all(&workspace_dir).expect("workspace dir should exist");
        fs::write(
            workspace_dir.join("main.tf"),
            r#"terraform {
  cloud {
    hostname = "yaffle.dev"
  }
}
"#,
        )
        .expect("fixture file should be written");
        let previous_cli_config = env::var_os("TF_CLI_CONFIG_FILE");
        env::remove_var("TF_CLI_CONFIG_FILE");
        env::set_var("TF_TOKEN_yaffle_dev", "test-token");

        let request = EngineRequest {
            operation: EngineOperation::Status,
            target: Some(EnvironmentTarget {
                environment: "main".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        };
        let repo_context = RepoContext {
            repo_root: repo.path().to_path_buf(),
            config_path: repo.path().join("yaffle.toml"),
            config: YaffleConfig {
                version: 1,
                environments: vec![yaffle_config::Environment {
                    name: "main".to_string(),
                }],
                workspaces: vec![yaffle_config::Workspace {
                    path: "infra/app".to_string(),
                    environments: yaffle_config::EnvironmentSelector::Named(vec![
                        "main".to_string()
                    ]),
                    automatic_preview_isolation: false,
                    variables: BTreeMap::new(),
                    outputs: BTreeMap::new(),
                    activation: Vec::new(),
                    verification: Vec::new(),
                }],
                cloud: yaffle_config::CloudConfig::default(),
            },
            current_namespace: Some("test-org--fixture".to_string()),
        };
        let prepared_repo = prepare_execution_repo(repo.path(), "infra/app", &request)
            .expect("prepared repo should exist");
        let auth = prepare_workspace_auth(
            &request,
            &repo_context,
            &prepared_repo,
            "infra/app",
            &prepared_repo.repo_root.join("infra/app"),
            false,
        )
        .expect("workspace auth should be prepared");

        env::remove_var("TF_TOKEN_yaffle_dev");
        if let Some(previous_cli_config) = previous_cli_config {
            env::set_var("TF_CLI_CONFIG_FILE", previous_cli_config);
        }

        assert_eq!(auth.required_hosts, vec!["yaffle.dev".to_string()]);
        assert!(auth.missing_hosts.is_empty());
        assert!(auth
            .resolved_hosts
            .iter()
            .any(|resolved| resolved.host == "yaffle.dev"
                && resolved.source == AuthCredentialSource::EnvToken));

        let config_path = auth
            .tf_cli_config_file
            .as_ref()
            .expect("auth config file should be generated");
        let config = fs::read_to_string(config_path).expect("auth config should be readable");
        assert!(config.contains("yaffle.dev"));
        assert!(config.contains("test-token"));
    }

    #[test]
    fn applies_module_api_host_override_in_prepared_workspace() {
        let repo = TempDir::new().expect("temp dir should exist");
        let _guard = crate::local_first::LOCAL_FIRST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let workspace_dir = repo.path().join("infra/app");
        fs::create_dir_all(&workspace_dir).expect("workspace dir should exist");
        fs::write(
            workspace_dir.join("main.tf"),
            r#"terraform {
  cloud {
    hostname = "yaffle.dev"
  }
}

module "shared" {
  source = "yaffle.dev/test-org--fixture/infra--shared/yaffle"
}
"#,
        )
        .expect("fixture file should be written");

        let previous_module_api_host = env::var_os(MODULE_API_HOST_OVERRIDE_ENV_VAR);
        env::set_var(MODULE_API_HOST_OVERRIDE_ENV_VAR, "yaffle.local:6969");

        let request = EngineRequest {
            operation: EngineOperation::Status,
            target: Some(EnvironmentTarget {
                environment: "main".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        };
        let prepared_repo = prepare_execution_repo(repo.path(), "infra/app", &request)
            .expect("prepared repo should exist");
        let workspace = yaffle_config::Workspace {
            path: "infra/app".to_string(),
            environments: yaffle_config::EnvironmentSelector::Named(vec!["main".to_string()]),
            automatic_preview_isolation: false,
            variables: BTreeMap::new(),
            outputs: BTreeMap::new(),
            activation: Vec::new(),
            verification: Vec::new(),
        };

        let mut automatic_workspace = workspace.clone();
        automatic_workspace.automatic_preview_isolation = true;
        assert!(automatic_preview_isolation_requires_cloud_review(
            &automatic_workspace,
            EnvironmentKind::Transient,
        ));
        assert!(!automatic_preview_isolation_requires_cloud_review(
            &automatic_workspace,
            EnvironmentKind::Named,
        ));
        let repo_context = RepoContext {
            repo_root: repo.path().to_path_buf(),
            config_path: repo.path().join("yaffle.toml"),
            config: YaffleConfig {
                version: 1,
                environments: vec![yaffle_config::Environment {
                    name: "main".to_string(),
                }],
                workspaces: vec![workspace.clone()],
                cloud: yaffle_config::CloudConfig::default(),
            },
            current_namespace: Some("test-org--fixture".to_string()),
        };

        let error = configure_workspace_execution(
            &request,
            &repo_context,
            &prepared_repo,
            &automatic_workspace,
            EnvironmentKind::Transient,
        )
        .expect_err("local transient execution should require Cloud review");
        assert_eq!(
            error.error.code,
            "automatic_preview_isolation_cloud_review_required"
        );

        let execution = configure_workspace_execution(
            &request,
            &repo_context,
            &prepared_repo,
            &workspace,
            EnvironmentKind::Named,
        )
        .expect("workspace execution should be prepared");

        if let Some(previous_module_api_host) = previous_module_api_host {
            env::set_var(MODULE_API_HOST_OVERRIDE_ENV_VAR, previous_module_api_host);
        } else {
            env::remove_var(MODULE_API_HOST_OVERRIDE_ENV_VAR);
        }

        let rewritten = fs::read_to_string(prepared_repo.repo_root.join("infra/app/main.tf"))
            .expect("rewritten workspace file should be readable");
        let tfvars = fs::read_to_string(
            prepared_repo
                .repo_root
                .join("infra/app/yaffle.auto.tfvars.json"),
        )
        .expect("tfvars file should be readable");

        assert!(!rewritten.contains(CANONICAL_YAFFLE_MODULE_HOST));
        assert!(tfvars.contains("module_registry_host"));
        assert_eq!(
            execution.auth.required_hosts,
            vec!["yaffle.local:6969".to_string()]
        );
    }

    #[test]
    fn applies_module_api_host_override_to_lifecycle_webhook_urls() {
        let _guard = crate::local_first::LOCAL_FIRST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let previous_module_api_host = env::var_os(MODULE_API_HOST_OVERRIDE_ENV_VAR);
        env::set_var(MODULE_API_HOST_OVERRIDE_ENV_VAR, "yaffle.local:6969");

        let rewritten = rewrite_canonical_yaffle_host_url(
            "https://yaffle.dev/api/lifecycle/hooks/preview-ready",
        );

        if let Some(previous_module_api_host) = previous_module_api_host {
            env::set_var(MODULE_API_HOST_OVERRIDE_ENV_VAR, previous_module_api_host);
        } else {
            env::remove_var(MODULE_API_HOST_OVERRIDE_ENV_VAR);
        }

        assert_eq!(
            rewritten,
            "https://yaffle.local:6969/api/lifecycle/hooks/preview-ready"
        );
    }

    #[test]
    fn derives_github_repository_dispatch_url_from_namespace() {
        let github = LifecycleGitHubRepositoryDispatch {
            owner: None,
            repo: None,
            event_type: "yaffle.activation".to_string(),
            api_url: None,
        };

        let url = github_repository_dispatch_url(&github, Some("yaffle-dot-dev--yaffle"))
            .expect("dispatch url should be derived");

        assert_eq!(
            url,
            "https://api.github.com/repos/yaffle-dot-dev/yaffle/dispatches"
        );
    }

    #[test]
    fn builds_control_plane_dispatch_body_for_github_repository_dispatch() {
        let hook = LifecycleHook {
            key: "control-plane".to_string(),
            environments: vec!["main".to_string()],
            kind: yaffle_config::LifecycleHookKind::GitHubRepositoryDispatch,
            timeout: Some("30m".to_string()),
            failure: LifecycleFailurePolicy::Failed,
            scopes: vec!["usable".to_string()],
            dispatch: LifecycleHookDispatch::GitHubRepositoryDispatch(
                LifecycleGitHubRepositoryDispatch {
                    owner: Some("yaffle-dot-dev".to_string()),
                    repo: Some("yaffle".to_string()),
                    event_type: "yaffle.activation".to_string(),
                    api_url: None,
                },
            ),
        };

        let payload = json!({
            "repo_namespace": "yaffle-dot-dev--yaffle",
            "environment": "main",
            "workspace_path": "apps/control-plane/infra",
            "item_key": "control-plane",
            "phase": "activation",
            "outputs": {},
            "on_completion": "https://yaffle.dev/api/lifecycle/completions/token-1",
        });

        let body = lifecycle_control_plane_dispatch_body(
            &hook,
            payload,
            "run-1",
            "item-1",
            "main",
            "apps/control-plane/infra",
            "activation",
        )
        .expect("dispatch body should build");

        assert_eq!(body["dispatch"]["kind"], "github_repository_dispatch");
        assert_eq!(body["dispatch"]["github"]["eventType"], "yaffle.activation");
        assert_eq!(body["payload"]["item_key"], "control-plane");
        assert_eq!(body["itemId"], "item-1");
    }

    #[test]
    fn classifies_unreachable_init_failure_before_missing_auth() {
        let classification = classify_tofu_command_failure(
            "tofu_init_failed",
            "apps/marketing/infra",
            "Failed to request discovery document: connect: connection refused",
            &PreparedWorkspaceAuth {
                required_hosts: vec!["yaffle.local:6969".to_string()],
                missing_hosts: vec!["yaffle.local:6969".to_string()],
                ..PreparedWorkspaceAuth::default()
            },
        )
        .expect("init failure should be classified");

        assert_eq!(classification.0, "auth_host_unreachable");
    }

    #[test]
    fn prepare_tf_login_exports_mints_shell_session_credential() {
        let repo = TempDir::new().expect("temp dir should exist");
        let temp_home = TempDir::new().expect("temp dir should exist");
        let _guard = crate::local_first::LOCAL_FIRST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        fs::create_dir_all(repo.path().join(".git")).expect("git dir should exist");
        fs::write(
            repo.path().join(".git/config"),
            "[remote \"origin\"]\n  url = https://github.com/test-org/fixture.git\n",
        )
        .expect("git config should exist");
        fs::write(
            repo.path().join("yaffle.toml"),
            r#"version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "apps/web/infra"
environments = ["main"]
"#,
        )
        .expect("config should be written");
        write_workspace_file(
            repo.path(),
            "apps/web/infra/main.tf",
            "locals { ready = true }\n",
        );

        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let authority = listener
            .local_addr()
            .expect("listener should have address")
            .to_string();
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("connection should accept");
                let mut buffer = [0_u8; 8192];
                let bytes_read = stream.read(&mut buffer).expect("request should read");
                let request = String::from_utf8_lossy(&buffer[..bytes_read]);

                if request.starts_with("POST /api/sessions/anonymous HTTP/1.1") {
                    assert!(!request.contains("feature-token:"));
                    let body = r#"{"data":{"principal_id":"principal-test","session_id":"session-test","token":"principal-token-test","issued_at":"2026-04-28T00:00:00Z","expires_at":"2030-04-28T00:00:00Z"}}"#;
                    write_test_response(&mut stream, body);
                } else if request.starts_with("POST /api/execution-tokens HTTP/1.1") {
                    assert!(request.contains("authorization: Bearer principal-token-test"));
                    assert!(request.contains(r#""sessionKind":"shell_session""#));
                    let body = r#"{"data":{"token":"execution-token-shell-session","repo_binding_id":"binding-test","expires_at":"2030-04-28T04:00:00Z"}}"#;
                    write_test_response(&mut stream, body);
                } else {
                    panic!("unexpected request: {request}");
                }
            }
        });

        let previous_home = env::var_os("HOME");
        let previous_module_api_host = env::var_os(MODULE_API_HOST_OVERRIDE_ENV_VAR);

        env::set_var("HOME", temp_home.path());
        env::set_var(
            MODULE_API_HOST_OVERRIDE_ENV_VAR,
            format!("http://{authority}"),
        );

        let exports = prepare_tf_login_exports(repo.path(), "main", "apps/web/infra")
            .expect("tf login exports should be prepared");

        if let Some(previous_home) = previous_home {
            env::set_var("HOME", previous_home);
        } else {
            env::remove_var("HOME");
        }
        if let Some(previous_module_api_host) = previous_module_api_host {
            env::set_var(MODULE_API_HOST_OVERRIDE_ENV_VAR, previous_module_api_host);
        } else {
            env::remove_var(MODULE_API_HOST_OVERRIDE_ENV_VAR);
        }

        server.join().expect("server thread should exit cleanly");

        let config_path = exports
            .lines()
            .find(|line| line.starts_with("export TF_CLI_CONFIG_FILE="))
            .expect("tf login exports should include TF_CLI_CONFIG_FILE")
            .trim_start_matches("export TF_CLI_CONFIG_FILE=")
            .trim_matches('\'');
        let config =
            fs::read_to_string(config_path).expect("tf login credentials file should exist");

        assert!(config.contains("execution-token-shell-session"));
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(config_path)
                .expect("credential metadata should exist")
                .permissions()
                .mode()
                & 0o777,
            0o600,
        );
        assert!(exports.contains("export TF_DATA_DIR='"));
        assert!(exports.contains("export TF_CLI_ARGS_plan='-state="));
        assert!(exports.contains("export TF_CLI_ARGS_apply='-state="));
        assert!(exports.contains("export TF_CLI_ARGS_destroy='-state="));
        assert!(exports.contains("export TF_CLI_ARGS_output='-state="));
        assert!(exports.contains(".yaffle/state/main/apps/web/infra/terraform.tfstate"));
        assert!(exports.contains(".yaffle/tf-data/main/apps/web/infra"));
    }

    #[test]
    fn doctor_reports_repo_health_for_valid_config() {
        let repo = TempDir::new().expect("temp dir should exist");
        let _guard = TOFU_OVERRIDE_LOCK.lock().expect("lock should succeed");
        let tofu_path = write_fake_tofu(repo.path().join("fake-tofu"), "1.11.5");
        env::set_var(TOFU_OVERRIDE_ENV_VAR, &tofu_path);

        fs::create_dir_all(repo.path().join(".git")).expect("git dir should exist");
        fs::write(
            repo.path().join(".git/config"),
            "[remote \"origin\"]\n  url = https://github.com/acme/platform.git\n",
        )
        .expect("git config should exist");

        fs::write(
            repo.path().join("yaffle.toml"),
            r#"version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "infra/shared"
environments = ["main"]

[[workspaces]]
path = "apps/web/infra"
environments = ["main"]
"#,
        )
        .expect("config should be written");

        write_workspace_file(
            repo.path(),
            "infra/shared/main.tf",
            "resource \"null_resource\" \"x\" {}\n",
        );
        write_workspace_file(
            repo.path(),
            "apps/web/infra/main.tf",
            r#"module "shared" {
  source = "yaffle.dev/acme--platform/infra--shared/yaffle"
}
"#,
        );

        let response = execute(
            &EngineRequest {
                operation: EngineOperation::Doctor,
                target: None,
                selection: WorkspaceSelection::default(),
                wait_for: None,
            },
            repo.path(),
        )
        .expect("doctor should execute");

        env::remove_var(TOFU_OVERRIDE_ENV_VAR);

        assert_ne!(response.result.kind, OperationResultKind::Failed);
        assert!(response.result.summary.contains("doctor"));
        assert!(response.result.summary.contains("[ok] resolved tofu via"));
        assert!(response.result.summary.contains("[ok] loaded config"));
        assert!(response
            .result
            .summary
            .contains("[ok] resolved static graph"));
        assert!(response
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("environment_graph_resolved")));
    }

    #[test]
    fn doctor_reports_missing_workspace_directories() {
        let repo = TempDir::new().expect("temp dir should exist");
        let _guard = TOFU_OVERRIDE_LOCK.lock().expect("lock should succeed");
        let tofu_path = write_fake_tofu(repo.path().join("fake-tofu"), "1.11.5");
        env::set_var(TOFU_OVERRIDE_ENV_VAR, &tofu_path);

        fs::write(
            repo.path().join("yaffle.toml"),
            r#"version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "infra/missing"
environments = ["main"]
"#,
        )
        .expect("config should be written");

        let response = execute(
            &EngineRequest {
                operation: EngineOperation::Doctor,
                target: None,
                selection: WorkspaceSelection::default(),
                wait_for: None,
            },
            repo.path(),
        )
        .expect("doctor should execute");

        env::remove_var(TOFU_OVERRIDE_ENV_VAR);

        assert_eq!(response.result.kind, OperationResultKind::Failed);
        assert!(response
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("workspace_directories_missing")));
    }

    #[test]
    fn doctor_reports_missing_config_without_engine_error() {
        let repo = TempDir::new().expect("temp dir should exist");
        let _guard = TOFU_OVERRIDE_LOCK.lock().expect("lock should succeed");
        let tofu_path = write_fake_tofu(repo.path().join("fake-tofu"), "1.11.5");
        env::set_var(TOFU_OVERRIDE_ENV_VAR, &tofu_path);

        let response = execute(
            &EngineRequest {
                operation: EngineOperation::Doctor,
                target: None,
                selection: WorkspaceSelection::default(),
                wait_for: None,
            },
            repo.path(),
        )
        .expect("doctor should convert missing config into a report");

        env::remove_var(TOFU_OVERRIDE_ENV_VAR);

        assert_eq!(response.result.kind, OperationResultKind::Failed);
        assert!(response
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("config_not_found")));
        assert!(response.result.summary.contains("[fail]"));
    }

    #[test]
    fn injects_missing_environment_variable_declarations() {
        let repo = TempDir::new().expect("temp dir should exist");
        let workspace_dir = repo.path().join("infra/app");
        fs::create_dir_all(&workspace_dir).expect("workspace dir should exist");
        fs::write(
            workspace_dir.join("main.tf"),
            "locals {\n  descriptor = \"${var.environment}:${var.environment_kind}\"\n}\n",
        )
        .expect("fixture file should be written");

        ensure_injected_variable_declarations(
            &EngineRequest {
                operation: EngineOperation::Converge,
                target: Some(EnvironmentTarget {
                    environment: "main".to_string(),
                }),
                selection: WorkspaceSelection::default(),
                wait_for: None,
            },
            &workspace_dir,
        )
        .expect("missing variable declarations should be injected");

        let content = fs::read_to_string(workspace_dir.join("yaffle_injected_variables.tf"))
            .expect("injected variables file should exist");
        assert!(content.contains("variable \"environment\""));
        assert!(content.contains("variable \"environment_kind\""));
    }

    #[test]
    fn derives_repo_namespace_from_remote_url() {
        assert_eq!(
            namespace_from_remote_url("https://github.com/yaffle-dot-dev/yaffle.git"),
            Some("yaffle-dot-dev--yaffle".to_string()),
        );
        assert_eq!(
            namespace_from_remote_url("git@github.com:acme/platform.git"),
            Some("acme--platform".to_string()),
        );
    }

    fn write_workspace_file(repo_root: &Path, relative_path: &str, content: &str) {
        let path = repo_root.join(relative_path);
        fs::create_dir_all(path.parent().expect("parent directory should exist"))
            .expect("directories should be created");
        fs::write(path, content).expect("workspace file should be written");
    }

    fn write_test_response(stream: &mut std::net::TcpStream, body: &str) {
        let response = format!(
            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("response should write");
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
