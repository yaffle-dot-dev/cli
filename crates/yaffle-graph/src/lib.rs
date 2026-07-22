use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use hcl::eval::{Context, Evaluate};
use hcl::{Attribute, Body, Value};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use yaffle_config::{EnvironmentSelector, VariableValue, Workspace, YaffleConfig};
use yaffle_contracts::WorkspaceSelection;

const DEFAULT_ALLOWED_MODULE_HOSTS: [&str; 2] = ["yaffle.dev", ".ts.net"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceNode {
    pub path: String,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentKind {
    Named,
    Transient,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedWorkspaceGraph {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_kind: Option<EnvironmentKind>,
    pub workspaces: Vec<WorkspaceNode>,
}

impl ResolvedWorkspaceGraph {
    pub fn workspace_paths(&self) -> Vec<String> {
        self.workspaces
            .iter()
            .map(|workspace| workspace.path.clone())
            .collect()
    }

    pub fn workspace(&self, path: &str) -> Option<&WorkspaceNode> {
        self.workspaces
            .iter()
            .find(|workspace| workspace.path == path)
    }

    pub fn topological_order(&self) -> Result<Vec<String>, GraphError> {
        let mut order = Vec::new();
        let mut permanent = BTreeSet::new();
        let mut temporary = Vec::new();

        for workspace in &self.workspaces {
            visit_workspace(
                &workspace.path,
                self,
                &mut permanent,
                &mut temporary,
                &mut order,
            )?;
        }

        Ok(order)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DependencyScannerOptions {
    pub allowed_hosts: Option<Vec<String>>,
    pub current_namespace: Option<String>,
    pub variables: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceGraphOptions {
    pub allowed_hosts: Option<Vec<String>>,
    pub current_namespace: Option<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum GraphError {
    #[error("workspace selection references unknown workspace '{workspace}'")]
    UnknownWorkspace { workspace: String },
    #[error("workspace '{workspace}' is not active in environment '{environment}'")]
    WorkspaceNotInEnvironment {
        workspace: String,
        environment: String,
    },
    #[error("workspace dependency cycle detected: {}", .cycle_path.join(" -> "))]
    DependencyCycle { cycle_path: Vec<String> },
}

pub fn environment_kind_for_name(config: &YaffleConfig, environment: &str) -> EnvironmentKind {
    if config
        .environments
        .iter()
        .any(|candidate| candidate.name == environment)
    {
        EnvironmentKind::Named
    } else {
        EnvironmentKind::Transient
    }
}

pub fn get_workspaces_for_environment(config: &YaffleConfig, environment: &str) -> Vec<String> {
    let kind = environment_kind_for_name(config, environment);

    config
        .workspaces
        .iter()
        .filter(|workspace| workspace_matches_environment(workspace, environment, kind))
        .map(|workspace| workspace.path.clone())
        .collect()
}

pub fn resolve_workspace_graph(
    repo_root: &Path,
    config: &YaffleConfig,
    environment: Option<&str>,
    options: &WorkspaceGraphOptions,
) -> Result<ResolvedWorkspaceGraph, GraphError> {
    let environment_kind = environment.map(|name| environment_kind_for_name(config, name));

    let resolved_workspaces: Vec<&Workspace> = match (environment, environment_kind) {
        (Some(environment), Some(kind)) => config
            .workspaces
            .iter()
            .filter(|workspace| workspace_matches_environment(workspace, environment, kind))
            .collect(),
        _ => config.workspaces.iter().collect(),
    };

    let known_workspaces = resolved_workspaces
        .iter()
        .map(|workspace| workspace.path.clone())
        .collect::<BTreeSet<_>>();

    let mut workspaces = Vec::new();
    for workspace in resolved_workspaces {
        workspaces.push(WorkspaceNode {
            path: workspace.path.clone(),
            dependencies: scan_workspace_dependencies(
                repo_root,
                workspace,
                &known_workspaces,
                options,
            ),
        });
    }

    let graph = ResolvedWorkspaceGraph {
        environment: environment.map(ToOwned::to_owned),
        environment_kind,
        workspaces,
    };

    graph.topological_order()?;

    Ok(graph)
}

pub fn apply_workspace_selection(
    config: &YaffleConfig,
    graph: &ResolvedWorkspaceGraph,
    selection: &WorkspaceSelection,
) -> Result<ResolvedWorkspaceGraph, GraphError> {
    if selection.is_empty() {
        return Ok(graph.clone());
    }

    let declared_workspaces = config
        .workspaces
        .iter()
        .map(|workspace| workspace.path.as_str())
        .collect::<BTreeSet<_>>();

    let selected_workspaces = selection
        .workspaces
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();

    let available_workspaces = graph
        .workspaces
        .iter()
        .map(|workspace| workspace.path.as_str())
        .collect::<BTreeSet<_>>();

    for workspace in &selection.workspaces {
        if !declared_workspaces.contains(workspace.as_str()) {
            return Err(GraphError::UnknownWorkspace {
                workspace: workspace.clone(),
            });
        }

        if !available_workspaces.contains(workspace.as_str()) {
            return Err(GraphError::WorkspaceNotInEnvironment {
                workspace: workspace.clone(),
                environment: graph
                    .environment
                    .clone()
                    .unwrap_or_else(|| "static graph".to_string()),
            });
        }
    }

    Ok(ResolvedWorkspaceGraph {
        environment: graph.environment.clone(),
        environment_kind: graph.environment_kind,
        workspaces: graph
            .workspaces
            .iter()
            .filter(|workspace| selected_workspaces.contains(&workspace.path))
            .map(|workspace| WorkspaceNode {
                path: workspace.path.clone(),
                dependencies: workspace
                    .dependencies
                    .iter()
                    .filter(|dependency| selected_workspaces.contains(*dependency))
                    .cloned()
                    .collect(),
            })
            .collect(),
    })
}

pub fn extract_dependencies_from_content(
    content: &str,
    options: &DependencyScannerOptions,
) -> Vec<String> {
    let Ok(body) = hcl::from_str::<Body>(content) else {
        return Vec::new();
    };

    extract_dependencies_from_body(&body, options)
}

pub fn module_name_to_workspace_path(module_name: &str) -> String {
    module_name.replace("--", "/")
}

pub fn workspace_path_to_module_name(workspace_path: &str) -> String {
    workspace_path.replace('/', "--")
}

fn visit_workspace(
    path: &str,
    graph: &ResolvedWorkspaceGraph,
    permanent: &mut BTreeSet<String>,
    temporary: &mut Vec<String>,
    order: &mut Vec<String>,
) -> Result<(), GraphError> {
    if permanent.contains(path) {
        return Ok(());
    }

    if let Some(start) = temporary.iter().position(|entry| entry == path) {
        return Err(GraphError::DependencyCycle {
            cycle_path: temporary[start..].to_vec(),
        });
    }

    temporary.push(path.to_string());

    if let Some(workspace) = graph.workspace(path) {
        for dependency in &workspace.dependencies {
            visit_workspace(dependency, graph, permanent, temporary, order)?;
        }
    }

    temporary.pop();
    permanent.insert(path.to_string());
    order.push(path.to_string());

    Ok(())
}

fn workspace_matches_environment(
    workspace: &Workspace,
    environment: &str,
    environment_kind: EnvironmentKind,
) -> bool {
    match &workspace.environments {
        EnvironmentSelector::All => true,
        EnvironmentSelector::Named(_) if environment_kind == EnvironmentKind::Transient => false,
        EnvironmentSelector::Named(names) => names.iter().any(|name| name == environment),
    }
}

fn scan_workspace_dependencies(
    repo_root: &Path,
    workspace: &Workspace,
    known_workspaces: &BTreeSet<String>,
    options: &WorkspaceGraphOptions,
) -> Vec<String> {
    let workspace_dir = repo_root.join(&workspace.path);
    if !workspace_dir.is_dir() {
        return Vec::new();
    }

    let mut terraform_files = Vec::new();
    find_terraform_files(&workspace_dir, &mut terraform_files);

    let body = read_workspace_body(&terraform_files);

    let dependencies = extract_dependencies_from_body(
        &body,
        &DependencyScannerOptions {
            allowed_hosts: options.allowed_hosts.clone(),
            current_namespace: options.current_namespace.clone(),
            variables: workspace_variables_to_values(&workspace.variables),
        },
    );

    let mut seen = BTreeSet::new();
    let mut filtered = Vec::new();
    for dependency in dependencies {
        if dependency != workspace.path
            && known_workspaces.contains(&dependency)
            && seen.insert(dependency.clone())
        {
            filtered.push(dependency);
        }
    }

    filtered
}

fn read_workspace_body(terraform_files: &[PathBuf]) -> Body {
    let mut body = Body::default();

    for file in terraform_files {
        let Ok(content) = fs::read_to_string(file) else {
            continue;
        };

        let Ok(parsed) = hcl::from_str::<Body>(&content) else {
            continue;
        };

        body.extend(parsed);
    }

    body
}

fn extract_dependencies_from_body(body: &Body, options: &DependencyScannerOptions) -> Vec<String> {
    let context = build_terraform_context(body, &options.variables);
    let allowed_hosts = get_allowed_module_hosts(options.allowed_hosts.as_deref());
    let mut dependencies = Vec::new();

    for block in body.blocks() {
        if block.identifier() != "module" || block.labels().len() != 1 {
            continue;
        }

        let Some(source_attribute) = attribute_named(block.body(), "source") else {
            continue;
        };

        let Ok(source_value) = source_attribute.expr.evaluate(&context) else {
            continue;
        };

        let Some(source) = source_value.as_str() else {
            continue;
        };

        let Some(workspace_path) = parse_yaffle_module_workspace_path(
            source,
            &allowed_hosts,
            options.current_namespace.as_deref(),
        ) else {
            continue;
        };

        dependencies.push(workspace_path);
    }

    dependencies
}

fn build_terraform_context(
    body: &Body,
    provided_variables: &BTreeMap<String, Value>,
) -> Context<'static> {
    let mut resolved_variables = provided_variables.clone();
    let mut resolved_locals = BTreeMap::new();
    let mut pending_variables = BTreeMap::new();
    let mut pending_locals = BTreeMap::new();

    for block in body.blocks() {
        match (block.identifier(), block.labels()) {
            ("variable", [label]) => {
                let name = label.as_str().to_string();
                if resolved_variables.contains_key(&name) {
                    continue;
                }

                let Some(default_attribute) = attribute_named(block.body(), "default") else {
                    continue;
                };

                pending_variables
                    .entry(name)
                    .or_insert_with(|| default_attribute.expr.clone());
            }
            ("locals", []) => {
                for attribute in block.body.attributes() {
                    pending_locals.insert(attribute.key().to_string(), attribute.expr.clone());
                }
            }
            _ => {}
        }
    }

    let mut made_progress = true;
    while made_progress {
        made_progress = false;

        let variable_names = pending_variables.keys().cloned().collect::<Vec<_>>();
        for name in variable_names {
            let Some(expr) = pending_variables.get(&name).cloned() else {
                continue;
            };

            let context = terraform_eval_context(&resolved_variables, &resolved_locals);
            let Ok(value) = expr.evaluate(&context) else {
                continue;
            };

            resolved_variables.insert(name.clone(), value);
            pending_variables.remove(&name);
            made_progress = true;
        }

        let local_names = pending_locals.keys().cloned().collect::<Vec<_>>();
        for name in local_names {
            let Some(expr) = pending_locals.get(&name).cloned() else {
                continue;
            };

            let context = terraform_eval_context(&resolved_variables, &resolved_locals);
            let Ok(value) = expr.evaluate(&context) else {
                continue;
            };

            resolved_locals.insert(name.clone(), value);
            pending_locals.remove(&name);
            made_progress = true;
        }
    }

    terraform_eval_context(&resolved_variables, &resolved_locals)
}

fn terraform_eval_context(
    resolved_variables: &BTreeMap<String, Value>,
    resolved_locals: &BTreeMap<String, Value>,
) -> Context<'static> {
    let mut context = Context::new();

    for (name, value) in resolved_variables {
        context.declare_var(name.clone(), value.clone());
    }

    for (name, value) in resolved_locals {
        context.declare_var(name.clone(), value.clone());
    }

    context.declare_var(
        "var",
        Value::from_iter(
            resolved_variables
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        ),
    );
    context.declare_var(
        "local",
        Value::from_iter(
            resolved_locals
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        ),
    );

    context
}

fn attribute_named<'a>(body: &'a Body, name: &str) -> Option<&'a Attribute> {
    body.attributes().find(|attribute| attribute.key() == name)
}

fn find_terraform_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if name.starts_with('.') || name == "node_modules" {
            continue;
        }

        if path.is_dir() {
            find_terraform_files(&path, files);
            continue;
        }

        if path.extension().and_then(|extension| extension.to_str()) == Some("tf") {
            files.push(path);
        }
    }
}

fn workspace_variables_to_values(
    variables: &BTreeMap<String, VariableValue>,
) -> BTreeMap<String, Value> {
    variables
        .iter()
        .map(|(name, value)| {
            let rendered = match value {
                VariableValue::String(value) => Value::from(value.clone()),
                VariableValue::Integer(value) => Value::from(*value),
                VariableValue::Float(value) => Value::from(*value),
                VariableValue::Boolean(value) => Value::from(*value),
            };

            (name.clone(), rendered)
        })
        .collect()
}

fn get_allowed_module_hosts(allowed_hosts: Option<&[String]>) -> Vec<String> {
    if let Some(allowed_hosts) = allowed_hosts {
        return allowed_hosts
            .iter()
            .map(|host| host.trim().to_lowercase())
            .filter(|host| !host.is_empty())
            .collect();
    }

    if let Ok(value) = env::var("YAFFLE_MODULE_SOURCE_ALLOWED_HOSTS") {
        let parsed = value
            .split(',')
            .map(|entry| entry.trim().to_lowercase())
            .filter(|entry| !entry.is_empty())
            .collect::<Vec<_>>();

        if !parsed.is_empty() {
            return parsed;
        }
    }

    DEFAULT_ALLOWED_MODULE_HOSTS
        .iter()
        .map(|host| host.to_string())
        .collect()
}

fn is_allowed_module_host(host: &str, allowed_hosts: &[String]) -> bool {
    let normalized_host = host.trim().to_lowercase();

    allowed_hosts.iter().any(|allowed_host| {
        if let Some(suffix) = allowed_host.strip_prefix('.') {
            normalized_host == suffix || normalized_host.ends_with(allowed_host)
        } else {
            normalized_host == *allowed_host
        }
    })
}

fn parse_yaffle_module_workspace_path(
    source: &str,
    allowed_hosts: &[String],
    current_namespace: Option<&str>,
) -> Option<String> {
    if source.contains("://") {
        return None;
    }

    let parts = source.split('/').collect::<Vec<_>>();
    if parts.len() != 4 {
        return None;
    }

    let host = parts[0].split(':').next()?;
    let namespace = parts[1];
    let module_name = parts[2];
    let provider = parts[3].split('?').next()?;

    if provider != "yaffle"
        || !namespace.contains("--")
        || !is_allowed_module_host(host, allowed_hosts)
    {
        return None;
    }

    if let Some(current_namespace) = current_namespace {
        if normalize_namespace(namespace) != normalize_namespace(current_namespace) {
            return None;
        }
    }

    Some(module_name_to_workspace_path(module_name))
}

fn normalize_namespace(namespace: &str) -> String {
    namespace.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use yaffle_config::parse_yaffle_toml;

    #[test]
    fn converts_module_names_to_workspace_paths() {
        assert_eq!(
            module_name_to_workspace_path("apps--web--infra"),
            "apps/web/infra"
        );
        assert_eq!(
            module_name_to_workspace_path("infra--shared"),
            "infra/shared"
        );
        assert_eq!(module_name_to_workspace_path("single"), "single");
    }

    #[test]
    fn converts_workspace_paths_to_module_names() {
        assert_eq!(
            workspace_path_to_module_name("apps/web/infra"),
            "apps--web--infra"
        );
        assert_eq!(
            workspace_path_to_module_name("infra/shared"),
            "infra--shared"
        );
        assert_eq!(workspace_path_to_module_name("single"), "single");
    }

    #[test]
    fn extracts_dependencies_from_module_sources() {
        let content = r#"
module "shared" {
  source = "yaffle.dev/yaffle-dot-dev--yaffle/infra--shared/yaffle"
}

module "production" {
  source = "yaffle.dev/yaffle-dot-dev--yaffle/infra--production/yaffle"
}
"#;

        let dependencies =
            extract_dependencies_from_content(content, &DependencyScannerOptions::default());

        assert_eq!(
            dependencies,
            vec!["infra/shared".to_string(), "infra/production".to_string()],
        );
    }

    #[test]
    fn resolves_variable_defaults_and_locals_in_module_sources() {
        let content = r#"
variable "registry_host" {
  default = "yaffle.dev"
}

locals {
  selected_host = var.registry_host
}

module "shared" {
  source = "${local.selected_host}/org--repo/infra--shared/yaffle"
}
"#;

        let dependencies =
            extract_dependencies_from_content(content, &DependencyScannerOptions::default());

        assert_eq!(dependencies, vec!["infra/shared".to_string()]);
    }

    #[test]
    fn evaluates_hcl_traversals_in_module_sources() {
        let content = r#"
locals {
  selected_source = "yaffle.dev/org--repo/infra--nonprod/yaffle"
}

module "core" {
  source = local.selected_source
}
"#;

        let dependencies =
            extract_dependencies_from_content(content, &DependencyScannerOptions::default());

        assert_eq!(dependencies, vec!["infra/nonprod".to_string()]);
    }

    #[test]
    fn filters_dependencies_to_current_namespace() {
        let content = r#"
module "same_repo" {
  source = "yaffle.dev/yaffle-dot-dev--platform/core--network/yaffle"
}

module "cross_repo" {
  source = "yaffle.dev/yaffle-dot-dev--applications/core--network/yaffle"
}
"#;

        let dependencies = extract_dependencies_from_content(
            content,
            &DependencyScannerOptions {
                current_namespace: Some("yaffle-dot-dev--platform".to_string()),
                ..DependencyScannerOptions::default()
            },
        );

        assert_eq!(dependencies, vec!["core/network".to_string()]);
    }

    #[test]
    fn ignores_non_allowlisted_hosts() {
        let content = r#"
module "shared" {
  source = "evil.example.com/org--repo/infra--shared/yaffle"
}
"#;

        let dependencies =
            extract_dependencies_from_content(content, &DependencyScannerOptions::default());

        assert!(dependencies.is_empty());
    }

    #[test]
    fn resolves_named_environment_graph() {
        let repo = TempDir::new().expect("temp dir should exist");
        write_workspace_file(
            &repo,
            "apps/web/infra/data.tf",
            r#"
module "shared" {
  source = "yaffle.dev/org--repo/infra--shared/yaffle"
}
"#,
        );
        write_workspace_file(
            &repo,
            "apps/docs/infra/data.tf",
            r#"
module "shared" {
  source = "yaffle.dev/org--repo/infra--shared/yaffle"
}
"#,
        );
        write_workspace_file(
            &repo,
            "infra/shared/main.tf",
            "resource \"null_resource\" \"x\" {}\n",
        );

        let config = parse_config(
            r#"
version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "infra/shared"
environments = ["main"]

[[workspaces]]
path = "apps/web/infra"
environments = ["*"]

[[workspaces]]
path = "apps/docs/infra"
environments = ["main"]
"#,
        );

        let graph = resolve_workspace_graph(
            repo.path(),
            &config,
            Some("main"),
            &WorkspaceGraphOptions {
                current_namespace: Some("org--repo".to_string()),
                ..WorkspaceGraphOptions::default()
            },
        )
        .expect("named environment graph should resolve");

        assert_eq!(graph.environment_kind, Some(EnvironmentKind::Named));
        assert_eq!(
            graph.workspace_paths(),
            vec![
                "infra/shared".to_string(),
                "apps/web/infra".to_string(),
                "apps/docs/infra".to_string(),
            ],
        );
        assert_eq!(
            graph.workspace("apps/web/infra").unwrap().dependencies,
            vec!["infra/shared"],
        );
        assert_eq!(
            graph.workspace("apps/docs/infra").unwrap().dependencies,
            vec!["infra/shared"],
        );
        assert_eq!(
            graph.topological_order().unwrap(),
            vec![
                "infra/shared".to_string(),
                "apps/web/infra".to_string(),
                "apps/docs/infra".to_string(),
            ],
        );
    }

    #[test]
    fn resolves_transient_environment_graph_with_only_wildcard_workspaces() {
        let repo = TempDir::new().expect("temp dir should exist");
        write_workspace_file(
            &repo,
            "apps/web/infra/data.tf",
            r#"
module "shared" {
  source = "yaffle.dev/org--repo/infra--shared/yaffle"
}
"#,
        );
        write_workspace_file(
            &repo,
            "infra/shared/main.tf",
            "resource \"null_resource\" \"x\" {}\n",
        );

        let config = parse_config(
            r#"
version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "infra/shared"
environments = ["main"]

[[workspaces]]
path = "apps/web/infra"
environments = ["*"]
"#,
        );

        let graph = resolve_workspace_graph(
            repo.path(),
            &config,
            Some("pr-42"),
            &WorkspaceGraphOptions {
                current_namespace: Some("org--repo".to_string()),
                ..WorkspaceGraphOptions::default()
            },
        )
        .expect("transient environment graph should resolve");

        assert_eq!(graph.environment_kind, Some(EnvironmentKind::Transient));
        assert_eq!(graph.workspace_paths(), vec!["apps/web/infra".to_string()]);
        assert!(graph
            .workspace("apps/web/infra")
            .unwrap()
            .dependencies
            .is_empty());
    }

    #[test]
    fn applies_workspace_selection_without_duplication() {
        let repo = TempDir::new().expect("temp dir should exist");
        write_workspace_file(
            &repo,
            "apps/web/infra/data.tf",
            r#"
module "shared" {
  source = "yaffle.dev/org--repo/infra--shared/yaffle"
}
"#,
        );
        write_workspace_file(
            &repo,
            "apps/docs/infra/data.tf",
            r#"
module "shared" {
  source = "yaffle.dev/org--repo/infra--shared/yaffle"
}
"#,
        );
        write_workspace_file(
            &repo,
            "infra/shared/main.tf",
            "resource \"null_resource\" \"x\" {}\n",
        );

        let config = parse_config(
            r#"
version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "infra/shared"
environments = ["main"]

[[workspaces]]
path = "apps/web/infra"
environments = ["main"]

[[workspaces]]
path = "apps/docs/infra"
environments = ["main"]
"#,
        );

        let graph = resolve_workspace_graph(
            repo.path(),
            &config,
            Some("main"),
            &WorkspaceGraphOptions {
                current_namespace: Some("org--repo".to_string()),
                ..WorkspaceGraphOptions::default()
            },
        )
        .expect("graph should resolve");

        let selected = apply_workspace_selection(
            &config,
            &graph,
            &WorkspaceSelection {
                workspaces: vec![
                    "apps/web/infra".to_string(),
                    "infra/shared".to_string(),
                    "apps/web/infra".to_string(),
                ],
            },
        )
        .expect("selection should succeed");

        assert_eq!(
            selected.workspace_paths(),
            vec!["infra/shared".to_string(), "apps/web/infra".to_string()],
        );
        assert_eq!(
            selected.workspace("apps/web/infra").unwrap().dependencies,
            vec!["infra/shared"],
        );
    }

    #[test]
    fn rejects_unknown_workspace_selection() {
        let graph = ResolvedWorkspaceGraph {
            environment: Some("main".to_string()),
            environment_kind: Some(EnvironmentKind::Named),
            workspaces: vec![WorkspaceNode {
                path: "infra/shared".to_string(),
                dependencies: Vec::new(),
            }],
        };
        let config = parse_config(
            r#"
version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "infra/shared"
environments = ["main"]
"#,
        );

        let error = apply_workspace_selection(
            &config,
            &graph,
            &WorkspaceSelection {
                workspaces: vec!["apps/missing/infra".to_string()],
            },
        )
        .expect_err("unknown workspace should fail");

        assert_eq!(
            error,
            GraphError::UnknownWorkspace {
                workspace: "apps/missing/infra".to_string(),
            },
        );
    }

    #[test]
    fn rejects_workspace_selection_outside_environment() {
        let repo = TempDir::new().expect("temp dir should exist");
        write_workspace_file(
            &repo,
            "apps/web/infra/main.tf",
            "resource \"null_resource\" \"x\" {}\n",
        );
        write_workspace_file(
            &repo,
            "infra/shared/main.tf",
            "resource \"null_resource\" \"x\" {}\n",
        );

        let config = parse_config(
            r#"
version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "infra/shared"
environments = ["main"]

[[workspaces]]
path = "apps/web/infra"
environments = ["*"]
"#,
        );

        let graph = resolve_workspace_graph(
            repo.path(),
            &config,
            Some("pr-42"),
            &WorkspaceGraphOptions::default(),
        )
        .expect("graph should resolve");

        let error = apply_workspace_selection(
            &config,
            &graph,
            &WorkspaceSelection {
                workspaces: vec!["infra/shared".to_string()],
            },
        )
        .expect_err("selection outside transient graph should fail");

        assert_eq!(
            error,
            GraphError::WorkspaceNotInEnvironment {
                workspace: "infra/shared".to_string(),
                environment: "pr-42".to_string(),
            },
        );
    }

    #[test]
    fn detects_workspace_dependency_cycles() {
        let repo = TempDir::new().expect("temp dir should exist");
        write_workspace_file(
            &repo,
            "infra/a/main.tf",
            r#"
module "b" {
  source = "yaffle.dev/org--repo/infra--b/yaffle"
}
"#,
        );
        write_workspace_file(
            &repo,
            "infra/b/main.tf",
            r#"
module "a" {
  source = "yaffle.dev/org--repo/infra--a/yaffle"
}
"#,
        );

        let config = parse_config(
            r#"
version = 1

[[environments]]
name = "main"

[[workspaces]]
path = "infra/a"
environments = ["main"]

[[workspaces]]
path = "infra/b"
environments = ["main"]
"#,
        );

        let error = resolve_workspace_graph(
            repo.path(),
            &config,
            Some("main"),
            &WorkspaceGraphOptions {
                current_namespace: Some("org--repo".to_string()),
                ..WorkspaceGraphOptions::default()
            },
        )
        .expect_err("cycle should fail graph resolution");

        assert_eq!(
            error,
            GraphError::DependencyCycle {
                cycle_path: vec!["infra/a".to_string(), "infra/b".to_string()],
            },
        );
    }

    fn parse_config(input: &str) -> YaffleConfig {
        parse_yaffle_toml(input).expect("config should parse")
    }

    fn write_workspace_file(repo: &TempDir, relative_path: &str, content: &str) {
        let path = repo.path().join(relative_path);
        fs::create_dir_all(path.parent().expect("parent directory should exist"))
            .expect("directories should be created");
        fs::write(path, content).expect("file should be written");
    }
}
