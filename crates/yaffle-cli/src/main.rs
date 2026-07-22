// Keep the versioned CLI error document inline until the next contract revision.
#![allow(clippy::result_large_err)]

use std::fmt::{Display, Formatter};
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use sha2::{Digest, Sha256};

use yaffle_contracts::{
    EngineError, EngineOperation, EngineResponse, EnvironmentTarget, ErrorPayload,
    OperationResultKind, WorkspaceSelection, CONTRACT_VERSION,
};
use yaffle_engine::{
    build_cloud_cli_authorize_url, clear_local_cloud_auth, compute_local_repo_fingerprint,
    exchange_cloud_cli_login_code, execute, get_cloud_remote_converge_status,
    load_local_cloud_auth_status, prepare_tf_login_exports, start_cloud_remote_converge,
    CloudCliLoginResult, CloudRemoteConvergeHandle, CloudRemoteConvergeRequest,
    CloudRemoteConvergeStatus, CloudRemoteLatestRunSummary, EngineRequest, LocalCloudAuthStatus,
    StoredPrincipalCredential, StoredPrincipalType,
};

const CLOUD_LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

type CliResult = Result<(), CliFailure>;

#[derive(Debug)]
struct CliFailure {
    json: bool,
    rendered: bool,
    payload: EngineError,
}

impl Display for CliFailure {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.payload.error.message)
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "yaffle",
    version,
    about = "Environment orchestration for Terraform/OpenTofu",
    long_about = "Yaffle CLI\n\nCreate, inspect, and destroy named or transient environments using the canonical Yaffle command surface.",
    after_help = "Examples:\n  yaffle converge --env main\n  yaffle outputs --env main\n  yaffle outputs --env main --workspace apps/control-plane/infra\n  yaffle graph --env pr-7\n  eval \"$(yaffle tf login --env main --workspace apps/web/infra)\"\n  yaffle cloud login"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Converge an environment to the current desired configuration and revision
    Converge(ConvergeCommand),
    /// Destroy an environment or a targeted subset in dependency-safe reverse order
    Destroy(TargetedCommand),
    /// Show environment conditions, materialization, and derived status information
    Status(EnvironmentOnlyCommand),
    /// Wait for an environment condition to become met or settled
    Wait(WaitCommand),
    /// Read outputs for a workspace in an environment
    Outputs(OutputsCommand),
    /// Inspect the static or environment-resolved workspace dependency graph
    Graph(GraphCommand),
    /// Diagnose local or cloud prerequisites, configuration, and capability problems
    Doctor(JsonCommand),
    #[command(subcommand)]
    /// Bootstrap raw tofu access for the current shell
    Tf(TfCommands),
    /// Generate shell completion scripts for the static CLI surface
    Completion(CompletionCommand),
    #[command(subcommand)]
    Cloud(CloudCommands),
}

#[derive(Debug, Args)]
struct TargetedCommand {
    /// Environment name
    #[arg(long)]
    env: String,
    /// Workspace path to target. Repeat to select multiple workspaces.
    #[arg(long = "workspace")]
    workspaces: Vec<String>,
    /// Emit machine-readable JSON output
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ConvergeCommand {
    /// Environment name
    #[arg(long)]
    env: String,
    /// Workspace path to target. Repeat to select multiple workspaces.
    #[arg(long = "workspace")]
    workspaces: Vec<String>,
    /// Emit machine-readable JSON output
    #[arg(long)]
    json: bool,
    /// Run the converge through Yaffle's hosted paid-cloud execution path
    #[arg(long)]
    remote: bool,
}

#[derive(Debug, Args)]
struct EnvironmentOnlyCommand {
    /// Environment name
    #[arg(long)]
    env: String,
    /// Emit machine-readable JSON output
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct WaitCommand {
    /// Environment name
    #[arg(long)]
    env: String,
    /// Environment condition to wait for
    #[arg(long = "for")]
    condition: String,
    /// Emit machine-readable JSON output
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct OutputsCommand {
    /// Environment name
    #[arg(long)]
    env: String,
    /// Workspace path to narrow to. Repeat to select multiple workspaces.
    #[arg(long = "workspace")]
    workspaces: Vec<String>,
    /// Emit machine-readable JSON output
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct GraphCommand {
    /// Optional environment name for environment-resolved graph output
    #[arg(long)]
    env: Option<String>,
    /// Emit machine-readable JSON output
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct JsonCommand {
    /// Emit machine-readable JSON output
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct CompletionCommand {
    /// Shell to generate completion for
    #[arg(value_enum)]
    shell: Shell,
}

#[derive(Debug, Subcommand)]
enum CloudCommands {
    /// Authenticate the operator to Yaffle Cloud
    Login,
    /// Remove local Yaffle Cloud authentication state
    Logout,
    /// Show current Yaffle Cloud authentication/backend status
    Status,
}

#[derive(Debug, Subcommand)]
enum TfCommands {
    /// Emit shell exports so raw tofu can resolve Yaffle-hosted output modules
    Login(TfLoginCommand),
}

#[derive(Debug, Args)]
struct TfLoginCommand {
    /// Environment name
    #[arg(long)]
    env: String,
    /// Workspace path
    #[arg(long = "workspace")]
    workspace: String,
}

fn main() {
    if let Err(error) = run() {
        if error.rendered {
            // The command already emitted its stable result document.
        } else if error.json {
            match serde_json::to_string_pretty(&error.payload) {
                Ok(value) => println!("{value}"),
                Err(_) => eprintln!("Error: {}", error.payload.error.message),
            }
        } else {
            eprintln!("Error: {error}");
        }
        std::process::exit(1);
    }
}

fn run() -> CliResult {
    let cli = Cli::parse();

    match cli.command {
        None => {
            Cli::command().print_long_help().map_err(|error| {
                command_error(
                    false,
                    None,
                    None,
                    None,
                    "help_render_failed",
                    error.to_string(),
                )
            })?;
            println!();
            Ok(())
        }
        Some(Commands::Converge(command)) => run_converge(command),
        Some(Commands::Destroy(command)) => {
            run_targeted_operation(EngineOperation::Destroy, command)
        }
        Some(Commands::Status(command)) => {
            run_environment_operation(EngineOperation::Status, command)
        }
        Some(Commands::Wait(command)) => run_wait(command),
        Some(Commands::Outputs(command)) => run_outputs(command),
        Some(Commands::Graph(command)) => run_graph(command),
        Some(Commands::Doctor(command)) => run_doctor(command),
        Some(Commands::Tf(command)) => run_tf(command),
        Some(Commands::Completion(command)) => run_completion(command),
        Some(Commands::Cloud(command)) => run_cloud(command),
    }
}

fn run_targeted_operation(operation: EngineOperation, command: TargetedCommand) -> CliResult {
    run_engine_request(
        command.json,
        EngineRequest {
            operation,
            target: Some(EnvironmentTarget {
                environment: command.env,
            }),
            selection: WorkspaceSelection {
                workspaces: command.workspaces,
            },
            wait_for: None,
        },
    )
}

fn run_converge(command: ConvergeCommand) -> CliResult {
    let request = EngineRequest {
        operation: EngineOperation::Converge,
        target: Some(EnvironmentTarget {
            environment: command.env,
        }),
        selection: WorkspaceSelection {
            workspaces: command.workspaces,
        },
        wait_for: None,
    };

    if command.remote {
        return run_remote_converge(command.json, request);
    }

    run_engine_request(command.json, request)
}

fn run_remote_converge(json: bool, request: EngineRequest) -> CliResult {
    let working_dir = current_working_directory(json, &request)?;
    let principal = load_account_cloud_principal(json, &request)?;
    let remote_request = build_remote_converge_request(&working_dir, &request)?;
    let handle = start_cloud_remote_converge(&principal, &remote_request).map_err(|error| {
        command_error(
            json,
            Some(request.operation.clone()),
            request.target.clone(),
            Some(request.selection.clone()),
            "remote_converge_start_failed",
            error.friendly_message(),
        )
    })?;

    let status = follow_remote_converge(json, &request, &principal, &handle)?;

    if json {
        let failed = remote_status_failed(&status);
        let summary = if failed {
            remote_failure_message(&status)
        } else {
            format!(
                "Hosted converge completed successfully for {} (run group {}).",
                status.run_group.environment_name, status.run_group.id
            )
        };
        let document = serde_json::json!({
            "contract_version": CONTRACT_VERSION,
            "operation": &request.operation,
            "target": &request.target,
            "selection": &request.selection,
            "result": {
                "kind": if failed { "failed" } else { "succeeded" },
                "summary": summary,
            },
            "remote": &status,
        });
        let rendered = serde_json::to_string_pretty(&document).map_err(|error| {
            command_error(
                json,
                Some(request.operation.clone()),
                request.target.clone(),
                Some(request.selection.clone()),
                "json_render_failed",
                format!("Failed to render hosted converge result as JSON: {error}"),
            )
        })?;
        println!("{rendered}");
        if failed {
            return Err(command_error(
                json,
                Some(request.operation),
                request.target,
                Some(request.selection),
                "remote_converge_failed",
                remote_failure_message(&status),
            )
            .rendered());
        }
        return Ok(());
    }

    if remote_status_failed(&status) {
        return Err(command_error(
            json,
            Some(request.operation),
            request.target,
            Some(request.selection),
            "remote_converge_failed",
            remote_failure_message(&status),
        ));
    }

    println!(
        "Hosted converge completed successfully for {} (run group {}).",
        status.run_group.environment_name, status.run_group.id
    );
    Ok(())
}

fn load_account_cloud_principal(
    json: bool,
    request: &EngineRequest,
) -> Result<StoredPrincipalCredential, CliFailure> {
    let status = load_local_cloud_auth_status().map_err(|error| {
        command_error(
            json,
            Some(request.operation.clone()),
            request.target.clone(),
            Some(request.selection.clone()),
            "cloud_auth_unavailable",
            format!("Failed to read Yaffle Cloud auth state: {error}"),
        )
    })?;

    if status.expired {
        return Err(command_error(
            json,
            Some(request.operation.clone()),
            request.target.clone(),
            Some(request.selection.clone()),
            "cloud_auth_expired",
            "Your Yaffle Cloud account session has expired. Run `yaffle cloud login` again before using `--remote`.",
        ));
    }

    let Some(principal) = status.stored_principal else {
        return Err(command_error(
            json,
            Some(request.operation.clone()),
            request.target.clone(),
            Some(request.selection.clone()),
            "cloud_auth_required",
            "Remote converge requires a Yaffle Cloud account session. Run `yaffle cloud login` first.",
        ));
    };

    if principal.principal_type != StoredPrincipalType::Account {
        return Err(command_error(
            json,
            Some(request.operation.clone()),
            request.target.clone(),
            Some(request.selection.clone()),
            "paid_cloud_required",
            "Remote converge requires an account-backed paid-cloud session. Run `yaffle cloud login` first.",
        ));
    }

    Ok(principal)
}

fn build_remote_converge_request(
    working_dir: &Path,
    request: &EngineRequest,
) -> Result<CloudRemoteConvergeRequest, CliFailure> {
    ensure_clean_git_worktree(working_dir, request)?;
    let repo_full_name = infer_repo_full_name(working_dir, request)?;
    let head_sha = current_git_head_sha(working_dir, request)?;
    let git_ref = resolve_remote_git_ref(working_dir, request, &head_sha)?;
    let workspace_paths = request.selection.workspaces.clone();
    let environment_name = request
        .target
        .as_ref()
        .map(|target| target.environment.clone())
        .expect("remote converge requires a target environment");
    let local_repo_fingerprint = compute_local_repo_fingerprint(working_dir).map_err(|error| {
        command_error(
            false,
            Some(request.operation.clone()),
            request.target.clone(),
            Some(request.selection.clone()),
            "repo_fingerprint_failed",
            format!("Failed to compute the local repo fingerprint for remote converge: {error}"),
        )
    })?;
    let canonical_repo_namespace = repo_full_name.replace('/', "--");

    Ok(CloudRemoteConvergeRequest {
        repo_full_name,
        canonical_repo_namespace,
        local_repo_fingerprint,
        environment_name,
        git_ref,
        head_sha,
        workspace_paths,
    })
}

fn follow_remote_converge(
    json: bool,
    request: &EngineRequest,
    principal: &StoredPrincipalCredential,
    handle: &CloudRemoteConvergeHandle,
) -> Result<CloudRemoteConvergeStatus, CliFailure> {
    if !json {
        println!(
            "Hosted converge queued for {} (run group {}).",
            handle.environment_name, handle.run_group_id
        );
        println!("Selected workspaces: {}", handle.workspace_paths.join(", "));
        if let Some(web_url) = &handle.web_url {
            println!("View in Yaffle: {web_url}");
        }
    }

    let mut last_snapshot: Option<CloudRemoteConvergeStatus> = None;

    loop {
        let snapshot =
            get_cloud_remote_converge_status(principal, &handle.run_group_id).map_err(|error| {
                command_error(
                    json,
                    Some(request.operation.clone()),
                    request.target.clone(),
                    Some(request.selection.clone()),
                    "remote_converge_follow_failed",
                    error.friendly_message(),
                )
            })?;

        if !json {
            maybe_print_remote_snapshot(last_snapshot.as_ref(), &snapshot);
        }

        if remote_status_terminal(&snapshot.run_group.status) {
            return Ok(snapshot);
        }

        last_snapshot = Some(snapshot);
        thread::sleep(Duration::from_secs(2));
    }
}

fn maybe_print_remote_snapshot(
    previous: Option<&CloudRemoteConvergeStatus>,
    current: &CloudRemoteConvergeStatus,
) {
    if previous.map(remote_snapshot_signature).as_deref()
        == Some(remote_snapshot_signature(current).as_str())
    {
        return;
    }

    println!(
        "[hosted] run group {} -> {}",
        current.run_group.id, current.run_group.status
    );
    for deployment in &current.deployments {
        let run_label = deployment.latest_run.as_ref().map(remote_run_label);
        match run_label {
            Some(run_label) => println!(
                "  - {}: {} ({})",
                deployment.workspace_path, deployment.status, run_label
            ),
            None => println!("  - {}: {}", deployment.workspace_path, deployment.status),
        }
    }
}

fn remote_run_label(run: &CloudRemoteLatestRunSummary) -> String {
    if run.run_type == "apply" && run.status == "skipped" {
        return "apply not needed".to_string();
    }

    format!("{} {}", run.run_type, run.status)
}

fn remote_snapshot_signature(snapshot: &CloudRemoteConvergeStatus) -> String {
    let mut value = format!("{}:{}", snapshot.run_group.id, snapshot.run_group.status);
    for deployment in &snapshot.deployments {
        value.push_str(&format!(
            "|{}:{}:{}:{}",
            deployment.workspace_path,
            deployment.status,
            deployment
                .latest_run
                .as_ref()
                .map(|run| run.run_type.as_str())
                .unwrap_or("-"),
            deployment
                .latest_run
                .as_ref()
                .map(|run| run.status.as_str())
                .unwrap_or("-"),
        ));
    }
    value
}

fn remote_status_terminal(status: &str) -> bool {
    matches!(status, "success" | "failed" | "partial")
}

fn remote_status_failed(status: &CloudRemoteConvergeStatus) -> bool {
    matches!(status.run_group.status.as_str(), "failed" | "partial")
}

fn remote_failure_message(status: &CloudRemoteConvergeStatus) -> String {
    let failing = status.deployments.iter().find(|deployment| {
        deployment.status == "failed"
            || deployment
                .latest_run
                .as_ref()
                .is_some_and(|run| run.status == "failed")
    });

    if let Some(deployment) = failing {
        if let Some(run) = &deployment.latest_run {
            if let Some(log_output) = &run.log_output {
                let trimmed = log_output.trim();
                if !trimmed.is_empty() {
                    return format!(
                        "Hosted converge failed in {} during {}:\n{}",
                        deployment.workspace_path, run.run_type, trimmed
                    );
                }
            }
            if let Some(error) = &run.error_message {
                return format!(
                    "Hosted converge failed in {} during {}: {}",
                    deployment.workspace_path, run.run_type, error
                );
            }
        }
        return format!("Hosted converge failed in {}.", deployment.workspace_path);
    }

    format!(
        "Hosted converge finished with status '{}'.",
        status.run_group.status
    )
}

fn ensure_clean_git_worktree(
    working_dir: &Path,
    request: &EngineRequest,
) -> Result<(), CliFailure> {
    let output = Command::new("git")
        .arg("status")
        .arg("--porcelain")
        .current_dir(working_dir)
        .output()
        .map_err(|error| {
            command_error(
                false,
                Some(request.operation.clone()),
                request.target.clone(),
                Some(request.selection.clone()),
                "git_status_failed",
                format!("Failed to inspect git working tree state: {error}"),
            )
        })?;

    if !output.status.success() {
        return Err(command_error(
            false,
            Some(request.operation.clone()),
            request.target.clone(),
            Some(request.selection.clone()),
            "git_status_failed",
            "Failed to inspect git working tree state for remote converge.",
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        return Err(command_error(
            false,
            Some(request.operation.clone()),
            request.target.clone(),
            Some(request.selection.clone()),
            "dirty_worktree_not_supported",
            "Remote converge currently requires a clean working tree.",
        ));
    }

    Ok(())
}

fn infer_repo_full_name(working_dir: &Path, request: &EngineRequest) -> Result<String, CliFailure> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(working_dir)
        .output()
        .map_err(|error| {
            command_error(
                false,
                Some(request.operation.clone()),
                request.target.clone(),
                Some(request.selection.clone()),
                "repo_remote_unavailable",
                format!("Failed to resolve git remote origin: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(command_error(
            false,
            Some(request.operation.clone()),
            request.target.clone(),
            Some(request.selection.clone()),
            "repo_remote_unavailable",
            "Could not resolve git remote origin for remote converge.",
        ));
    }

    let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_github_repo_full_name(&remote).ok_or_else(|| {
        command_error(
            false,
            Some(request.operation.clone()),
            request.target.clone(),
            Some(request.selection.clone()),
            "repo_identity_unavailable",
            "Remote converge currently requires a GitHub origin remote.",
        )
    })
}

fn parse_github_repo_full_name(remote: &str) -> Option<String> {
    let trimmed = remote
        .strip_prefix("git@github.com:")
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))
        .or_else(|| remote.strip_prefix("https://github.com/"))?;
    let trimmed = trimmed.trim_end_matches(".git");
    let mut parts = trimmed.split('/');
    let owner = parts.next().unwrap_or("");
    let repo = parts.next().unwrap_or("");
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return None;
    }

    Some(format!("{owner}/{repo}"))
}

fn current_git_head_sha(working_dir: &Path, request: &EngineRequest) -> Result<String, CliFailure> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(working_dir)
        .output()
        .map_err(|error| {
            command_error(
                false,
                Some(request.operation.clone()),
                request.target.clone(),
                Some(request.selection.clone()),
                "git_sha_unavailable",
                format!("Failed to resolve git HEAD SHA for remote converge: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(command_error(
            false,
            Some(request.operation.clone()),
            request.target.clone(),
            Some(request.selection.clone()),
            "git_sha_unavailable",
            "Failed to resolve git HEAD SHA for remote converge.",
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn resolve_remote_git_ref(
    working_dir: &Path,
    request: &EngineRequest,
    local_head_sha: &str,
) -> Result<String, CliFailure> {
    let local_branch_ref = current_git_symbolic_ref(working_dir, request)?;
    if let Some(upstream_ref) = current_git_upstream_ref(working_dir, request)? {
        let upstream_sha = resolve_git_ref_sha(working_dir, request, &upstream_ref)?;
        if upstream_sha != local_head_sha {
            return Err(command_error(
                false,
                Some(request.operation.clone()),
                request.target.clone(),
                Some(request.selection.clone()),
                "unpushed_commits_not_supported",
                "Remote converge currently requires local HEAD to match the remote ref that will execute in the cloud. Push or fast-forward before using `--remote`.",
            ));
        }

        if let Some(mapped) = map_origin_remote_ref_to_head_ref(&upstream_ref) {
            return Ok(mapped);
        }
    }

    let remote_refs = list_origin_remote_refs(working_dir, request)?;
    if let Some(local_branch_ref) = local_branch_ref.as_deref() {
        let branch_name = local_branch_ref.trim_start_matches("refs/heads/");
        let candidate_remote_ref = format!("refs/remotes/origin/{branch_name}");
        if remote_refs
            .iter()
            .any(|(remote_ref, sha)| remote_ref == &candidate_remote_ref && sha == local_head_sha)
        {
            return Ok(local_branch_ref.to_string());
        }
    }

    let mut matching_remote_refs = remote_refs
        .into_iter()
        .filter(|(remote_ref, sha)| {
            sha == local_head_sha && remote_ref != "refs/remotes/origin/HEAD"
        })
        .filter_map(|(remote_ref, _)| map_origin_remote_ref_to_head_ref(&remote_ref))
        .collect::<Vec<_>>();
    matching_remote_refs.sort();
    matching_remote_refs.dedup();

    match matching_remote_refs.as_slice() {
        [resolved_ref] => Ok(resolved_ref.clone()),
        [] => Err(command_error(
            false,
            Some(request.operation.clone()),
            request.target.clone(),
            Some(request.selection.clone()),
            "remote_ref_unavailable",
            "Remote converge currently requires HEAD to be pushed to a resolvable origin branch or tag ref.",
        )),
        refs => Err(command_error(
            false,
            Some(request.operation.clone()),
            request.target.clone(),
            Some(request.selection.clone()),
            "remote_ref_ambiguous",
            format!(
                "Remote converge found multiple origin refs for HEAD ({}). Check out a branch or disambiguate the ref before using `--remote`.",
                refs.join(", ")
            ),
        )),
    }
}

fn current_git_symbolic_ref(
    working_dir: &Path,
    request: &EngineRequest,
) -> Result<Option<String>, CliFailure> {
    let output = Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .current_dir(working_dir)
        .output()
        .map_err(|error| {
            command_error(
                false,
                Some(request.operation.clone()),
                request.target.clone(),
                Some(request.selection.clone()),
                "git_ref_unavailable",
                format!("Failed to resolve git ref metadata for remote converge: {error}"),
            )
        })?;

    if !output.status.success() {
        return Ok(None);
    }

    Ok(Some(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

fn current_git_upstream_ref(
    working_dir: &Path,
    request: &EngineRequest,
) -> Result<Option<String>, CliFailure> {
    let output = Command::new("git")
        .args(["rev-parse", "--symbolic-full-name", "@{upstream}"])
        .current_dir(working_dir)
        .output()
        .map_err(|error| {
            command_error(
                false,
                Some(request.operation.clone()),
                request.target.clone(),
                Some(request.selection.clone()),
                "upstream_ref_unavailable",
                format!("Failed to inspect upstream ref metadata for remote converge: {error}"),
            )
        })?;

    if !output.status.success() {
        return Ok(None);
    }

    Ok(Some(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

fn resolve_git_ref_sha(
    working_dir: &Path,
    request: &EngineRequest,
    git_ref: &str,
) -> Result<String, CliFailure> {
    let output = Command::new("git")
        .args(["rev-parse", git_ref])
        .current_dir(working_dir)
        .output()
        .map_err(|error| {
            command_error(
                false,
                Some(request.operation.clone()),
                request.target.clone(),
                Some(request.selection.clone()),
                "git_ref_unavailable",
                format!("Failed to resolve git ref '{git_ref}' for remote converge: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(command_error(
            false,
            Some(request.operation.clone()),
            request.target.clone(),
            Some(request.selection.clone()),
            "git_ref_unavailable",
            format!("Failed to resolve git ref '{git_ref}' for remote converge."),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn list_origin_remote_refs(
    working_dir: &Path,
    request: &EngineRequest,
) -> Result<Vec<(String, String)>, CliFailure> {
    let output = Command::new("git")
        .args([
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/remotes/origin",
        ])
        .current_dir(working_dir)
        .output()
        .map_err(|error| {
            command_error(
                false,
                Some(request.operation.clone()),
                request.target.clone(),
                Some(request.selection.clone()),
                "remote_ref_unavailable",
                format!("Failed to inspect origin refs for remote converge: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(command_error(
            false,
            Some(request.operation.clone()),
            request.target.clone(),
            Some(request.selection.clone()),
            "remote_ref_unavailable",
            "Failed to inspect origin refs for remote converge.",
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_git_remote_ref_line)
        .collect())
}

fn parse_git_remote_ref_line(line: &str) -> Option<(String, String)> {
    let mut parts = line.split_whitespace();
    let git_ref = parts.next()?.trim();
    let sha = parts.next()?.trim();
    if git_ref.is_empty() || sha.is_empty() {
        return None;
    }
    Some((git_ref.to_string(), sha.to_string()))
}

fn map_origin_remote_ref_to_head_ref(remote_ref: &str) -> Option<String> {
    remote_ref
        .strip_prefix("refs/remotes/origin/")
        .filter(|value| !value.is_empty() && *value != "HEAD")
        .map(|suffix| format!("refs/heads/{suffix}"))
}

fn run_environment_operation(
    operation: EngineOperation,
    command: EnvironmentOnlyCommand,
) -> CliResult {
    run_engine_request(
        command.json,
        EngineRequest {
            operation,
            target: Some(EnvironmentTarget {
                environment: command.env,
            }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        },
    )
}

fn run_wait(command: WaitCommand) -> CliResult {
    run_engine_request(
        command.json,
        EngineRequest {
            operation: EngineOperation::Wait,
            target: Some(EnvironmentTarget {
                environment: command.env,
            }),
            selection: WorkspaceSelection::default(),
            wait_for: Some(command.condition),
        },
    )
}

fn run_outputs(command: OutputsCommand) -> CliResult {
    run_engine_request(
        command.json,
        EngineRequest {
            operation: EngineOperation::Outputs,
            target: Some(EnvironmentTarget {
                environment: command.env,
            }),
            selection: WorkspaceSelection {
                workspaces: command.workspaces,
            },
            wait_for: None,
        },
    )
}

fn run_graph(command: GraphCommand) -> CliResult {
    run_engine_request(
        command.json,
        EngineRequest {
            operation: EngineOperation::Graph,
            target: command
                .env
                .map(|environment| EnvironmentTarget { environment }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        },
    )
}

fn run_doctor(command: JsonCommand) -> CliResult {
    run_engine_request(
        command.json,
        EngineRequest {
            operation: EngineOperation::Doctor,
            target: None,
            selection: WorkspaceSelection::default(),
            wait_for: None,
        },
    )
}

fn run_tf(command: TfCommands) -> CliResult {
    match command {
        TfCommands::Login(command) => run_tf_login(command),
    }
}

fn run_tf_login(command: TfLoginCommand) -> CliResult {
    let prior_cloud_auth_status = load_local_cloud_auth_status().ok();
    let working_dir = std::env::current_dir().map_err(|error| {
        command_error(
            false,
            None,
            Some(EnvironmentTarget {
                environment: command.env.clone(),
            }),
            Some(WorkspaceSelection {
                workspaces: vec![command.workspace.clone()],
            }),
            "current_directory_unavailable",
            format!("Failed to resolve the current working directory: {error}"),
        )
    })?;

    let exports = prepare_tf_login_exports(&working_dir, &command.env, &command.workspace)
        .map_err(|payload| CliFailure {
            json: false,
            rendered: false,
            payload,
        })?;

    maybe_print_guest_bootstrap_notice(prior_cloud_auth_status.as_ref());

    print!("{exports}");
    Ok(())
}

fn run_cloud(command: CloudCommands) -> CliResult {
    match command {
        CloudCommands::Login => run_cloud_login(),
        CloudCommands::Logout => run_cloud_logout(),
        CloudCommands::Status => run_cloud_status(),
    }
}

fn run_cloud_login() -> CliResult {
    let prior_status = load_local_cloud_auth_status().ok();
    let callback_listener = bind_cloud_login_listener().map_err(|error| {
        command_error(
            false,
            None,
            None,
            None,
            "cloud_login_failed",
            format!("Failed to open a local callback port for cloud login: {error}"),
        )
    })?;
    let callback_port = callback_listener
        .local_addr()
        .map_err(|error| {
            command_error(
                false,
                None,
                None,
                None,
                "cloud_login_failed",
                format!("Failed to resolve the local callback port: {error}"),
            )
        })?
        .port();

    let code_verifier = generate_pkce_verifier().map_err(|error| {
        command_error(
            false,
            None,
            None,
            None,
            "cloud_login_failed",
            format!("Failed to generate a secure login verifier: {error}"),
        )
    })?;
    let code_challenge = pkce_challenge_for_verifier(&code_verifier);
    let state = generate_pkce_verifier().map_err(|error| {
        command_error(
            false,
            None,
            None,
            None,
            "cloud_login_failed",
            format!("Failed to generate login state: {error}"),
        )
    })?;
    let redirect_uri = format!("http://localhost:{callback_port}/callback");
    let authorize_url = build_cloud_cli_authorize_url(&redirect_uri, &code_challenge, &state)
        .map_err(|error| {
            command_error(
                false,
                None,
                None,
                None,
                "cloud_login_failed",
                format!("Failed to build the cloud login URL: {error}"),
            )
        })?;

    eprintln!("Opening your browser for Yaffle Cloud login...");
    if !open_browser(&authorize_url) {
        eprintln!("Open this URL manually: {authorize_url}");
    } else {
        eprintln!("If the browser does not open, visit: {authorize_url}");
    }

    let callback = wait_for_cloud_login_callback(callback_listener, &state)
        .map_err(|error| command_error(false, None, None, None, "cloud_login_failed", error))?;
    let login = exchange_cloud_cli_login_code(
        &callback.code,
        &code_verifier,
        &redirect_uri,
        prior_status
            .as_ref()
            .and_then(|status| status.stored_principal.as_ref()),
    )
    .map_err(|error| {
        command_error(
            false,
            None,
            None,
            None,
            "cloud_login_failed",
            format!("Yaffle Cloud login failed: {error}"),
        )
    })?;

    let status = load_local_cloud_auth_status().map_err(|error| {
        command_error(
            false,
            None,
            None,
            None,
            "cloud_login_failed",
            format!(
                "Logged in successfully, but failed to inspect local Yaffle Cloud auth: {error}"
            ),
        )
    })?;

    println!(
        "{}",
        render_cloud_transition(&render_cloud_login_success(&login), &status)
    );
    Ok(())
}

fn run_cloud_status() -> CliResult {
    let status = load_local_cloud_auth_status().map_err(|error| {
        command_error(
            false,
            None,
            None,
            None,
            "cloud_status_failed",
            format!("Failed to inspect local Yaffle Cloud auth: {error}"),
        )
    })?;

    println!("{}", render_cloud_status(&status));
    Ok(())
}

fn run_cloud_logout() -> CliResult {
    let status = load_local_cloud_auth_status().map_err(|error| {
        command_error(
            false,
            None,
            None,
            None,
            "cloud_logout_failed",
            format!("Failed to inspect local Yaffle Cloud auth: {error}"),
        )
    })?;
    let removed = clear_local_cloud_auth().map_err(|error| {
        command_error(
            false,
            None,
            None,
            None,
            "cloud_logout_failed",
            format!("Failed to clear local Yaffle Cloud auth: {error}"),
        )
    })?;

    let action = if removed {
        match status.stored_principal.as_ref().map(|principal| principal.principal_type) {
            Some(StoredPrincipalType::Account) => "Signed this machine out of Yaffle Cloud. Run `yaffle cloud login` to connect your account again.".to_string(),
            _ => "Removed this machine's temporary Yaffle guest session. Run `yaffle converge` to start another guest session, or `yaffle cloud login` to connect your account.".to_string(),
        }
    } else {
        "This machine was already signed out of Yaffle Cloud.".to_string()
    };

    let current_status = load_local_cloud_auth_status().map_err(|error| {
        command_error(
            false,
            None,
            None,
            None,
            "cloud_logout_failed",
            format!(
                "Signed out successfully, but failed to inspect local Yaffle Cloud auth: {error}"
            ),
        )
    })?;

    println!("{}", render_cloud_transition(&action, &current_status));

    Ok(())
}

fn render_cloud_status(status: &LocalCloudAuthStatus) -> String {
    render_cloud_status_with_note(status, local_backend_access_note())
}

fn render_cloud_status_with_note(status: &LocalCloudAuthStatus, local_dev_note: &str) -> String {
    let local_dev_note = format!("Local dev note: {local_dev_note}");

    let Some(principal) = &status.stored_principal else {
        return format!(
            "Yaffle Cloud: not connected\n\nThis machine is not connected to Yaffle Cloud yet.\n\nWhat you can do next:\n- run `yaffle converge` to continue as a temporary guest\n- run `yaffle tf login` to use raw OpenTofu with Yaffle-backed auth\n- run `yaffle cloud login` to connect your account\n\n{}",
            local_dev_note,
        );
    };

    let expires_at = principal.expires_at.as_deref().unwrap_or("unknown");

    if principal.principal_type == StoredPrincipalType::Account {
        let identity = describe_identity(principal);
        if status.expired {
            return format!(
                "Yaffle Cloud: account session expired\n\nThis machine was previously connected as {identity}, but that session has expired.\n\nWhat you can do next:\n- run `yaffle cloud login` to reconnect your account\n\n{}",
                local_dev_note,
            );
        }

        return format!(
            "Yaffle Cloud: connected as {identity}\n\nThis machine is signed in to Yaffle Cloud and ready for local-first workflows.\n\nWhat you can do next:\n- run `yaffle converge`\n- run `yaffle tf login`\n\nSession expires: {expires_at}\n{}",
            local_dev_note,
        );
    }

    if status.expired {
        return format!(
            "Yaffle Cloud: temporary guest session expired\n\nThis machine's previous guest session has expired.\n\nWhat you can do next:\n- run `yaffle converge` to start a new temporary guest session\n- run `yaffle tf login` to use raw OpenTofu with Yaffle-backed auth\n- run `yaffle cloud login` to connect your account\n\n{}",
            local_dev_note,
        );
    }

    format!(
        "Yaffle Cloud: connected as a temporary guest\n\nThis machine can use Yaffle Cloud for hosted modules and local-first auth. Guest sessions stay on the machine where they were created.\n\nWhat you can do next:\n- run `yaffle converge`\n- run `yaffle tf login`\n- run `yaffle cloud login` to save this setup to your account\n\nGuest session expires: {expires_at}\n{}",
        local_dev_note,
    )
}

fn render_cloud_login_success(login: &CloudCliLoginResult) -> String {
    let principal = &login.principal;
    let identity = describe_identity(principal);

    if login.converted_from_anonymous {
        return format!(
            "Logged into Yaffle Cloud as {identity}. Your temporary guest setup on this machine has been upgraded to your account, and its hosted output modules came with it."
        );
    }

    format!(
        "Logged into Yaffle Cloud as {identity}. This machine is now connected to your account."
    )
}

fn render_cloud_transition(action: &str, status: &LocalCloudAuthStatus) -> String {
    format!("{action}\n\n{}", render_cloud_status(status))
}

fn run_completion(command: CompletionCommand) -> CliResult {
    let mut root = Cli::command();
    generate(command.shell, &mut root, "yaffle", &mut io::stdout());
    Ok(())
}

fn run_engine_request(json: bool, request: EngineRequest) -> CliResult {
    let prior_cloud_auth_status = if json {
        None
    } else {
        load_local_cloud_auth_status().ok()
    };
    let working_dir = current_working_directory(json, &request)?;
    let response = execute(&request, &working_dir).map_err(|payload| CliFailure {
        json,
        rendered: false,
        payload,
    })?;

    if !json {
        maybe_print_guest_bootstrap_notice(prior_cloud_auth_status.as_ref());
    }

    render_response(json, &response)
}

fn maybe_print_guest_bootstrap_notice(prior_status: Option<&LocalCloudAuthStatus>) {
    let Some(prior_status) = prior_status else {
        return;
    };

    let created_new_guest_session = prior_status.stored_principal.is_none() || prior_status.expired;
    if !created_new_guest_session {
        return;
    }

    let Ok(current_status) = load_local_cloud_auth_status() else {
        return;
    };
    let Some(principal) = current_status.stored_principal.as_ref() else {
        return;
    };
    if current_status.expired {
        return;
    }
    if principal.principal_type != StoredPrincipalType::AnonymousSession {
        return;
    }

    eprintln!(
        "Connected this machine to Yaffle Cloud as a temporary guest. Yaffle can now use hosted modules and local-first auth here. Run `yaffle cloud login` any time to save this setup to your account.",
    );
}

fn local_backend_access_note() -> &'static str {
    "Yaffle Cloud access is available when a cloud operation needs it."
}

fn describe_identity(principal: &yaffle_engine::StoredPrincipalCredential) -> String {
    match (
        principal.user_name.as_deref(),
        principal.user_email.as_deref(),
    ) {
        (Some(name), Some(email)) => format!("{name} <{email}>"),
        (Some(name), None) => name.to_string(),
        (None, Some(email)) => email.to_string(),
        (None, None) => principal.principal_id.clone(),
    }
}

fn bind_cloud_login_listener() -> io::Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", 0))
}

fn generate_pkce_verifier() -> io::Result<String> {
    let mut bytes = [0_u8; 48];
    getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

fn pkce_challenge_for_verifier(code_verifier: &str) -> String {
    let digest = Sha256::digest(code_verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        return Command::new("open").arg(url).status().is_ok();
    }
    #[cfg(target_os = "windows")]
    {
        return Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .status()
            .is_ok();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return Command::new("xdg-open").arg(url).status().is_ok();
    }
    #[allow(unreachable_code)]
    false
}

struct CloudLoginCallback {
    code: String,
}

fn wait_for_cloud_login_callback(
    listener: TcpListener,
    expected_state: &str,
) -> Result<CloudLoginCallback, String> {
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("failed to configure cloud login listener: {error}"))?;
    let deadline = Instant::now() + CLOUD_LOGIN_TIMEOUT;
    let mut callback_attempts = 0_u8;

    loop {
        if Instant::now() >= deadline {
            return Err("timed out waiting for the cloud login callback".to_string());
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                callback_attempts += 1;
                if callback_attempts > 64 {
                    return Err("too many invalid cloud login callbacks".to_string());
                }
                let read_timeout = deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(2));
                stream
                    .set_read_timeout(Some(read_timeout))
                    .map_err(|error| format!("failed to secure cloud login callback: {error}"))?;
                let mut buffer = [0_u8; 8192];
                let bytes_read = match stream.read(&mut buffer) {
                    Ok(0) => continue,
                    Ok(bytes_read) => bytes_read,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    Err(error) => {
                        return Err(format!("failed to read cloud login callback: {error}"));
                    }
                };
                let request = String::from_utf8_lossy(&buffer[..bytes_read]);
                let Some(request_line) = request.lines().next() else {
                    continue;
                };
                let mut parts = request_line.split_whitespace();
                if parts.next() != Some("GET") {
                    write_login_callback_response(
                        &mut stream,
                        405,
                        "Yaffle Cloud login pending",
                        "Only GET callbacks are accepted.",
                    )
                    .ok();
                    continue;
                }
                let Some(target) = parts.next() else {
                    continue;
                };
                let (path, query) = target.split_once('?').unwrap_or((target, ""));
                if path != "/callback" {
                    write_login_callback_response(
                        &mut stream,
                        404,
                        "Yaffle Cloud login failed",
                        "Unexpected callback path.",
                    )
                    .ok();
                    continue;
                }

                let mut code = None;
                let mut state = None;
                for pair in query.split('&') {
                    let mut pieces = pair.splitn(2, '=');
                    let key = pieces.next().unwrap_or("");
                    let value = pieces.next().unwrap_or("");
                    if key == "code" {
                        code = Some(value.to_string());
                    } else if key == "state" {
                        state = Some(value.to_string());
                    }
                }

                if state.as_deref() != Some(expected_state) {
                    write_login_callback_response(
                        &mut stream,
                        400,
                        "Yaffle Cloud login failed",
                        "State verification failed.",
                    )
                    .ok();
                    continue;
                }

                let Some(code) = code else {
                    write_login_callback_response(
                        &mut stream,
                        400,
                        "Yaffle Cloud login failed",
                        "Authorization code was missing.",
                    )
                    .ok();
                    continue;
                };

                write_login_callback_response(
                    &mut stream,
                    200,
                    "Yaffle Cloud login complete",
                    "You can return to your terminal.",
                )
                .ok();
                return Ok(CloudLoginCallback { code });
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err("timed out waiting for the cloud login callback".to_string());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                return Err(format!("failed to accept cloud login callback: {error}"));
            }
        }
    }
}

fn write_login_callback_response(
    stream: &mut std::net::TcpStream,
    status: u16,
    title: &str,
    message: &str,
) -> io::Result<()> {
    let body = format!(
        "<!DOCTYPE html><html><head><title>{title}</title><meta charset=\"utf-8\"></head><body style=\"font-family:system-ui;padding:32px;background:#09090b;color:#fafafa\"><h1 style=\"font-size:1rem\">{title}</h1><p style=\"color:#a1a1aa\">{message}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes())
}

fn render_response(json: bool, response: &EngineResponse) -> CliResult {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(response).map_err(|error| {
                command_error(
                    json,
                    Some(response.operation.clone()),
                    response.target.clone(),
                    Some(response.selection.clone()),
                    "serialization_failed",
                    error.to_string(),
                )
            })?
        );
    } else {
        println!("{}", response.result.summary);
    }

    if matches!(
        &response.result.kind,
        OperationResultKind::Blocked | OperationResultKind::Failed
    ) {
        return Err(command_error(
            json,
            Some(response.operation.clone()),
            response.target.clone(),
            Some(response.selection.clone()),
            "operation_failed",
            response.result.summary.clone(),
        )
        .rendered());
    }

    Ok(())
}

#[allow(dead_code)]
fn command_help_for_tests() -> clap::Command {
    Cli::command()
}

fn command_error(
    json: bool,
    operation: Option<EngineOperation>,
    target: Option<EnvironmentTarget>,
    selection: Option<WorkspaceSelection>,
    code: impl Into<String>,
    message: impl Into<String>,
) -> CliFailure {
    CliFailure {
        json,
        rendered: false,
        payload: EngineError {
            contract_version: CONTRACT_VERSION,
            operation,
            target,
            selection,
            error: ErrorPayload {
                code: code.into(),
                message: message.into(),
                details: None,
            },
        },
    }
}

impl CliFailure {
    fn rendered(mut self) -> Self {
        self.rendered = true;
        self
    }
}

fn current_working_directory(json: bool, request: &EngineRequest) -> Result<PathBuf, CliFailure> {
    std::env::current_dir().map_err(|error| {
        command_error(
            json,
            Some(request.operation.clone()),
            request.target.clone(),
            Some(request.selection.clone()),
            "current_directory_unavailable",
            format!("Failed to resolve the current working directory: {error}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_status(
        principal: Option<yaffle_engine::StoredPrincipalCredential>,
        expired: bool,
    ) -> LocalCloudAuthStatus {
        LocalCloudAuthStatus {
            auth_store_path: PathBuf::from("/tmp/principal.json"),
            stored_principal: principal,
            expired,
        }
    }

    fn guest_principal() -> yaffle_engine::StoredPrincipalCredential {
        yaffle_engine::StoredPrincipalCredential {
            principal_type: StoredPrincipalType::AnonymousSession,
            principal_id: "guest-principal-id".to_string(),
            session_id: Some("guest-session-id".to_string()),
            token: "guest-token".to_string(),
            issued_at: "2026-05-03T00:00:00Z".to_string(),
            expires_at: Some("2026-05-17T00:00:00Z".to_string()),
            user_id: None,
            user_email: None,
            user_name: None,
        }
    }

    fn account_principal() -> yaffle_engine::StoredPrincipalCredential {
        yaffle_engine::StoredPrincipalCredential {
            principal_type: StoredPrincipalType::Account,
            principal_id: "account-principal-id".to_string(),
            session_id: None,
            token: "account-token".to_string(),
            issued_at: "2026-05-03T00:00:00Z".to_string(),
            expires_at: Some("2026-06-03T00:00:00Z".to_string()),
            user_id: Some("user-id".to_string()),
            user_email: Some("alex@example.com".to_string()),
            user_name: Some("Alex".to_string()),
        }
    }

    #[test]
    fn cloud_status_not_connected_uses_product_language() {
        let rendered = render_cloud_status_with_note(
            &test_status(None, false),
            "this shell can reach the local Yaffle backend.",
        );

        assert!(rendered.starts_with("Yaffle Cloud: not connected"));
        assert!(rendered.contains("run `yaffle converge` to continue as a temporary guest"));
        assert!(rendered.contains("run `yaffle cloud login` to connect your account"));
        assert!(!rendered.contains("principal.json"));
    }

    #[test]
    fn cloud_status_guest_prioritizes_user_state_over_internal_ids() {
        let rendered = render_cloud_status_with_note(
            &test_status(Some(guest_principal()), false),
            "this shell can reach the local Yaffle backend.",
        );

        assert!(rendered.starts_with("Yaffle Cloud: connected as a temporary guest"));
        assert!(rendered.contains("run `yaffle cloud login` to save this setup to your account"));
        assert!(!rendered.contains("guest-principal-id"));
        assert!(!rendered.contains("guest-session-id"));
        assert!(!rendered.contains("principal.json"));
    }

    #[test]
    fn cloud_status_account_leads_with_identity() {
        let rendered = render_cloud_status_with_note(
            &test_status(Some(account_principal()), false),
            "this shell can reach the local Yaffle backend.",
        );

        assert!(rendered.starts_with("Yaffle Cloud: connected as Alex <alex@example.com>"));
        assert!(rendered.contains("This machine is signed in to Yaffle Cloud"));
        assert!(rendered.contains("Session expires: 2026-06-03T00:00:00Z"));
        assert!(!rendered.contains("account-principal-id"));
    }

    #[test]
    fn cloud_login_success_conversion_feels_like_an_upgrade() {
        let rendered = render_cloud_login_success(&CloudCliLoginResult {
            principal: account_principal(),
            converted_from_anonymous: true,
        });

        assert!(rendered.contains("Logged into Yaffle Cloud as Alex <alex@example.com>."));
        assert!(rendered.contains("upgraded to your account"));
    }

    #[test]
    fn cloud_transition_ends_in_the_status_view() {
        let status = test_status(Some(account_principal()), false);
        let rendered = render_cloud_transition("Connected successfully.", &status);

        assert!(rendered.starts_with(
            "Connected successfully.\n\nYaffle Cloud: connected as Alex <alex@example.com>"
        ));
        assert!(rendered.contains("This machine is signed in to Yaffle Cloud"));
    }

    #[test]
    fn parses_github_repo_full_names_from_common_origin_urls() {
        assert_eq!(
            parse_github_repo_full_name("git@github.com:yaffledev/yaffle.git"),
            Some("yaffledev/yaffle".to_string())
        );
        assert_eq!(
            parse_github_repo_full_name("https://github.com/yaffledev/yaffle.git"),
            Some("yaffledev/yaffle".to_string())
        );
        assert_eq!(
            parse_github_repo_full_name("https://example.com/repo.git"),
            None
        );
        assert_eq!(
            parse_github_repo_full_name("https://attacker.example/github.com/owner/repo.git"),
            None
        );
    }

    #[test]
    fn cloud_login_ignores_invalid_callbacks_until_state_matches() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener should bind");
        let address = listener.local_addr().expect("listener should have address");
        let waiter = thread::spawn(move || wait_for_cloud_login_callback(listener, "expected"));

        let mut invalid = std::net::TcpStream::connect(address).expect("invalid callback connects");
        invalid
            .write_all(b"GET /callback?code=wrong&state=attacker HTTP/1.1\r\n\r\n")
            .expect("invalid callback writes");
        drop(invalid);

        let mut valid = std::net::TcpStream::connect(address).expect("valid callback connects");
        valid
            .write_all(b"GET /callback?code=accepted&state=expected HTTP/1.1\r\n\r\n")
            .expect("valid callback writes");
        drop(valid);

        let callback = waiter
            .join()
            .expect("callback waiter should finish")
            .expect("valid callback should succeed");
        assert_eq!(callback.code, "accepted");
    }

    #[test]
    fn maps_origin_remote_refs_to_head_refs_for_remote_converge() {
        assert_eq!(
            map_origin_remote_ref_to_head_ref("refs/remotes/origin/main"),
            Some("refs/heads/main".to_string())
        );
        assert_eq!(
            map_origin_remote_ref_to_head_ref("refs/remotes/origin/feature/remote"),
            Some("refs/heads/feature/remote".to_string())
        );
        assert_eq!(
            map_origin_remote_ref_to_head_ref("refs/remotes/origin/HEAD"),
            None
        );
    }

    #[test]
    fn parses_remote_ref_lines_from_git_for_each_ref_output() {
        assert_eq!(
            parse_git_remote_ref_line(
                "refs/remotes/origin/main abcdef1234567890abcdef1234567890abcdef12"
            ),
            Some((
                "refs/remotes/origin/main".to_string(),
                "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            ))
        );
        assert_eq!(parse_git_remote_ref_line(""), None);
        assert_eq!(parse_git_remote_ref_line("refs/remotes/origin/main"), None);
    }
}
