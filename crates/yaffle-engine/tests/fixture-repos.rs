#![allow(clippy::result_large_err)]

mod support;

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::sync::{LazyLock, Mutex};
use std::thread;
use std::time::Duration;

use reqwest::blocking::Client;
use serde_json::json;
use yaffle_contracts::{EngineOperation, OperationResultKind, WorkspaceSelection};
use yaffle_engine::{execute, EngineRequest, EnvironmentTarget};

use support::{
    copy_fixture_repo, run_tofu_apply, run_tofu_output_json, run_tofu_output_json_for_env,
};

static WAIT_ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[test]
fn graph_dependency_chain_fixture_resolves_expected_order() {
    let repo = copy_fixture_repo("graph-dependency-chain");

    let response = execute(
        &EngineRequest {
            operation: EngineOperation::Graph,
            target: Some(EnvironmentTarget {
                environment: "main".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        },
        repo.path(),
    )
    .expect("graph fixture should resolve");

    assert_eq!(response.result.kind, OperationResultKind::Succeeded);
    assert_eq!(
        response
            .workspaces
            .iter()
            .map(|workspace| workspace.workspace_path.as_str())
            .collect::<Vec<_>>(),
        vec!["infra/shared", "apps/api/infra", "apps/web/infra"],
    );
}

#[test]
fn graph_env_split_fixture_filters_named_and_transient_membership() {
    let repo = copy_fixture_repo("graph-env-split");

    let named = execute(
        &EngineRequest {
            operation: EngineOperation::Graph,
            target: Some(EnvironmentTarget {
                environment: "main".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        },
        repo.path(),
    )
    .expect("named environment graph should resolve");

    let transient = execute(
        &EngineRequest {
            operation: EngineOperation::Graph,
            target: Some(EnvironmentTarget {
                environment: "pr-42".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        },
        repo.path(),
    )
    .expect("transient environment graph should resolve");

    assert_eq!(
        named
            .workspaces
            .iter()
            .map(|workspace| workspace.workspace_path.as_str())
            .collect::<Vec<_>>(),
        vec!["infra/named", "apps/shared/infra"],
    );
    assert_eq!(
        transient
            .workspaces
            .iter()
            .map(|workspace| workspace.workspace_path.as_str())
            .collect::<Vec<_>>(),
        vec!["apps/shared/infra"],
    );
}

#[test]
fn outputs_minimal_single_fixture_can_apply_and_emit_outputs() {
    let repo = copy_fixture_repo("outputs-minimal-single");

    run_tofu_apply(repo.path(), "infra/single");
    let outputs = run_tofu_output_json(repo.path(), "infra/single");

    assert_eq!(outputs["service_name"]["value"], json!("single-service"));
    assert_eq!(outputs["numbers"]["value"], json!([1, 2, 3]));
    assert_eq!(
        outputs["settings"]["value"],
        json!({ "enabled": true, "tier": "test" })
    );

    let response = execute(
        &EngineRequest {
            operation: EngineOperation::Outputs,
            target: Some(EnvironmentTarget {
                environment: "main".to_string(),
            }),
            selection: WorkspaceSelection {
                workspaces: vec!["infra/single".to_string()],
            },
            wait_for: None,
        },
        repo.path(),
    )
    .expect("outputs operation should resolve single-workspace outputs");

    assert_eq!(response.result.kind, OperationResultKind::Succeeded);
    assert_eq!(
        response.outputs["service_name"].value,
        json!("single-service")
    );
    assert_eq!(response.outputs["numbers"].value, json!([1, 2, 3]));
    assert_eq!(
        response.outputs["settings"].value,
        json!({ "enabled": true, "tier": "test" })
    );
    assert!(response.result.summary.contains("resolved 3 output(s)"));
}

#[test]
fn outputs_fixture_returns_empty_map_before_state_exists() {
    let repo = copy_fixture_repo("outputs-minimal-single");

    let response = execute(
        &EngineRequest {
            operation: EngineOperation::Outputs,
            target: Some(EnvironmentTarget {
                environment: "main".to_string(),
            }),
            selection: WorkspaceSelection {
                workspaces: vec!["infra/single".to_string()],
            },
            wait_for: None,
        },
        repo.path(),
    )
    .expect("outputs should still execute before state exists");

    assert_eq!(response.result.kind, OperationResultKind::Succeeded);
    assert!(response.outputs.is_empty());
    assert!(response.result.summary.contains("resolved 0 output(s)"));
}

#[test]
fn status_fixture_reports_absent_before_state_exists() {
    let repo = copy_fixture_repo("outputs-minimal-single");

    let response = execute(
        &EngineRequest {
            operation: EngineOperation::Status,
            target: Some(EnvironmentTarget {
                environment: "main".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        },
        repo.path(),
    )
    .expect("status should succeed without existing state");

    assert_eq!(response.result.kind, OperationResultKind::Succeeded);
    assert_eq!(
        response
            .environment
            .as_ref()
            .and_then(|environment| environment.materialization.as_deref()),
        Some("absent")
    );
    assert_eq!(
        response.workspaces[0].materialization.as_deref(),
        Some("absent")
    );
}

#[test]
fn outputs_remote_state_chain_fixture_supports_upstream_and_downstream_states() {
    let repo = copy_fixture_repo("outputs-remote-state-chain");

    run_tofu_apply(repo.path(), "infra/shared");
    run_tofu_apply(repo.path(), "apps/web/infra");
    let outputs = run_tofu_output_json(repo.path(), "apps/web/infra");

    assert_eq!(
        outputs["base_url"]["value"],
        json!("https://shared.internal")
    );
    assert_eq!(outputs["https_port"]["value"], json!(8443));
    assert_eq!(
        outputs["feature_flags"]["value"],
        json!(["auth", "cdn", "metrics"])
    );

    let response = execute(
        &EngineRequest {
            operation: EngineOperation::Outputs,
            target: Some(EnvironmentTarget {
                environment: "main".to_string(),
            }),
            selection: WorkspaceSelection {
                workspaces: vec!["apps/web/infra".to_string()],
            },
            wait_for: None,
        },
        repo.path(),
    )
    .expect("outputs operation should resolve downstream outputs");

    assert_eq!(response.result.kind, OperationResultKind::Succeeded);
    assert_eq!(
        response.outputs["base_url"].value,
        json!("https://shared.internal")
    );
    assert_eq!(response.outputs["https_port"].value, json!(8443));
    assert_eq!(
        response.outputs["feature_flags"].value,
        json!(["auth", "cdn", "metrics"])
    );
}

#[test]
fn outputs_without_workspace_selection_group_results_by_workspace() {
    let repo = copy_fixture_repo("outputs-remote-state-chain");

    run_tofu_apply(repo.path(), "infra/shared");
    run_tofu_apply(repo.path(), "apps/web/infra");

    let response = execute(
        &EngineRequest {
            operation: EngineOperation::Outputs,
            target: Some(EnvironmentTarget {
                environment: "main".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        },
        repo.path(),
    )
    .expect("outputs operation should resolve environment-wide outputs");

    assert_eq!(response.result.kind, OperationResultKind::Succeeded);
    assert!(response.outputs.is_empty());
    assert_eq!(
        response.workspace_outputs["infra/shared"]["domain"].value,
        json!("shared.internal")
    );
    assert_eq!(
        response.workspace_outputs["apps/web/infra"]["https_port"].value,
        json!(8443)
    );
    assert!(response
        .result
        .summary
        .contains("resolved 6 output(s) across 2 workspace(s) in environment 'main'"));
    assert!(response.result.summary.contains("infra/shared:"));
    assert!(response.result.summary.contains("apps/web/infra:"));
}

#[test]
fn converge_remote_state_chain_fixture_persists_outputs_for_downstream_workspace() {
    let repo = copy_fixture_repo("outputs-remote-state-chain");

    let converge = with_local_first_env_disabled(|| {
        execute(
            &EngineRequest {
                operation: EngineOperation::Converge,
                target: Some(EnvironmentTarget {
                    environment: "main".to_string(),
                }),
                selection: WorkspaceSelection::default(),
                wait_for: None,
            },
            repo.path(),
        )
    })
    .expect("converge should succeed for remote-state fixture");

    assert_eq!(converge.result.kind, OperationResultKind::Succeeded);
    assert!(converge.result.summary.contains("converged 2 workspace(s)"));

    let direct_outputs = run_tofu_output_json(repo.path(), "apps/web/infra");
    assert_eq!(
        direct_outputs["base_url"]["value"],
        json!("https://shared.internal")
    );

    let outputs = execute(
        &EngineRequest {
            operation: EngineOperation::Outputs,
            target: Some(EnvironmentTarget {
                environment: "main".to_string(),
            }),
            selection: WorkspaceSelection {
                workspaces: vec!["apps/web/infra".to_string()],
            },
            wait_for: None,
        },
        repo.path(),
    )
    .expect("outputs should read state produced by converge");

    assert_eq!(
        outputs.outputs["base_url"].value,
        json!("https://shared.internal")
    );
    assert_eq!(outputs.outputs["https_port"].value, json!(8443));
}

#[test]
fn converge_persists_partial_state_when_apply_fails() {
    let repo = copy_fixture_repo("converge-partial-apply-state");

    let error = with_local_first_env_disabled(|| {
        execute(
            &EngineRequest {
                operation: EngineOperation::Converge,
                target: Some(EnvironmentTarget {
                    environment: "main".to_string(),
                }),
                selection: WorkspaceSelection::default(),
                wait_for: None,
            },
            repo.path(),
        )
    })
    .expect_err("converge should report the failed apply");

    assert_eq!(error.error.code, "tofu_apply_failed");

    let state_path = repo
        .path()
        .join(".yaffle/state/main/infra/single/terraform.tfstate");
    let state = fs::read_to_string(&state_path).expect("partial state should persist");
    let state: serde_json::Value = serde_json::from_str(&state).expect("state should be JSON");
    #[cfg(unix)]
    {
        assert_eq!(
            fs::metadata(&state_path)
                .expect("state metadata should exist")
                .permissions()
                .mode()
                & 0o777,
            0o600,
        );
        for directory in [
            repo.path().join(".yaffle"),
            repo.path().join(".yaffle/state"),
            repo.path().join(".yaffle/state/main"),
            repo.path().join(".yaffle/state/main/infra"),
            repo.path().join(".yaffle/state/main/infra/single"),
        ] {
            assert_eq!(
                fs::metadata(&directory)
                    .expect("state directory metadata should exist")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700,
                "state directory '{}' should be private",
                directory.display(),
            );
        }
    }
    let lineage = state["lineage"]
        .as_str()
        .expect("partial state should contain a lineage")
        .to_string();
    let resource_id = state["resources"]
        .as_array()
        .expect("partial state should contain resources")
        .iter()
        .find(|resource| resource["name"] == "created_before_failure")
        .and_then(|resource| resource["instances"][0]["attributes"]["id"].as_str())
        .expect("the created resource should have an ID")
        .to_string();

    fs::write(
        repo.path().join("infra/single/main.tf"),
        r#"terraform {
  required_version = ">= 1.8.0"
}

resource "terraform_data" "created_before_failure" {
  input = "durable"
}
"#,
    )
    .expect("the recovered configuration should be writable");

    let recovered = with_local_first_env_disabled(|| {
        execute(
            &EngineRequest {
                operation: EngineOperation::Converge,
                target: Some(EnvironmentTarget {
                    environment: "main".to_string(),
                }),
                selection: WorkspaceSelection::default(),
                wait_for: None,
            },
            repo.path(),
        )
    })
    .expect("the recovered converge should succeed");

    assert_eq!(recovered.result.kind, OperationResultKind::Succeeded);

    let recovered_state = fs::read_to_string(state_path).expect("recovered state should persist");
    let recovered_state: serde_json::Value =
        serde_json::from_str(&recovered_state).expect("recovered state should be JSON");
    let recovered_resource_id = recovered_state["resources"]
        .as_array()
        .expect("recovered state should contain resources")
        .iter()
        .find(|resource| resource["name"] == "created_before_failure")
        .and_then(|resource| resource["instances"][0]["attributes"]["id"].as_str())
        .expect("the recovered resource should have an ID");

    assert_eq!(recovered_state["lineage"], lineage);
    assert_eq!(recovered_resource_id, resource_id);
}

#[test]
fn destroy_persists_partial_state_when_destroy_fails() {
    let repo = copy_fixture_repo("destroy-partial-state");

    let converge = with_local_first_env_disabled(|| {
        execute(
            &EngineRequest {
                operation: EngineOperation::Converge,
                target: Some(EnvironmentTarget {
                    environment: "main".to_string(),
                }),
                selection: WorkspaceSelection::default(),
                wait_for: None,
            },
            repo.path(),
        )
    })
    .expect("the fixture should converge before destroy");

    assert_eq!(converge.result.kind, OperationResultKind::Succeeded);

    let error = with_local_first_env_disabled(|| {
        execute(
            &EngineRequest {
                operation: EngineOperation::Destroy,
                target: Some(EnvironmentTarget {
                    environment: "main".to_string(),
                }),
                selection: WorkspaceSelection::default(),
                wait_for: None,
            },
            repo.path(),
        )
    })
    .expect_err("destroy should report the failed provisioner");

    assert_eq!(error.error.code, "tofu_destroy_failed");

    let state_path = repo
        .path()
        .join(".yaffle/state/main/infra/single/terraform.tfstate");
    let state = fs::read_to_string(&state_path).expect("partial destroy state should persist");

    assert!(state.contains("fails_during_destroy"));
    assert!(!state.contains("deleted_before_failure"));

    fs::write(
        repo.path().join("infra/single/main.tf"),
        r#"terraform {
  required_version = ">= 1.8.0"
}

resource "terraform_data" "fails_during_destroy" {
  input = "retained"
}
"#,
    )
    .expect("the recovered configuration should be writable");

    let recovered = with_local_first_env_disabled(|| {
        execute(
            &EngineRequest {
                operation: EngineOperation::Destroy,
                target: Some(EnvironmentTarget {
                    environment: "main".to_string(),
                }),
                selection: WorkspaceSelection::default(),
                wait_for: None,
            },
            repo.path(),
        )
    })
    .expect("the recovered destroy should succeed");

    assert_eq!(recovered.result.kind, OperationResultKind::Succeeded);
    assert!(
        !state_path.exists(),
        "successful destroy should remove state"
    );
}

#[test]
fn status_remote_state_chain_fixture_reports_partial_materialization() {
    let repo = copy_fixture_repo("outputs-remote-state-chain");

    run_tofu_apply(repo.path(), "infra/shared");

    let response = execute(
        &EngineRequest {
            operation: EngineOperation::Status,
            target: Some(EnvironmentTarget {
                environment: "main".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        },
        repo.path(),
    )
    .expect("status should succeed for partially materialized fixture");

    assert_eq!(response.result.kind, OperationResultKind::Succeeded);
    assert_eq!(
        response
            .environment
            .as_ref()
            .and_then(|environment| environment.materialization.as_deref()),
        Some("partially_present")
    );
    assert_eq!(
        response
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_path == "infra/shared")
            .and_then(|workspace| workspace.materialization.as_deref()),
        Some("present")
    );
    assert_eq!(
        response
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_path == "apps/web/infra")
            .and_then(|workspace| workspace.materialization.as_deref()),
        Some("absent")
    );
}

#[test]
fn status_degrades_when_one_workspace_cannot_initialize() {
    let repo = copy_fixture_repo("status-init-failure-mixed");

    run_tofu_apply(repo.path(), "infra/good");

    let response = execute(
        &EngineRequest {
            operation: EngineOperation::Status,
            target: Some(EnvironmentTarget {
                environment: "main".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        },
        repo.path(),
    )
    .expect("status should degrade instead of failing");

    assert_eq!(response.result.kind, OperationResultKind::Degraded);
    assert_eq!(
        response
            .environment
            .as_ref()
            .and_then(|environment| environment.materialization.as_deref()),
        Some("partially_present")
    );
    assert_eq!(
        response
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_path == "infra/good")
            .and_then(|workspace| workspace.materialization.as_deref()),
        Some("present")
    );
    assert_eq!(
        response
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_path == "apps/bad/infra")
            .and_then(|workspace| workspace.materialization.as_deref()),
        Some("partially_present")
    );
    assert!(response
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code.as_deref() == Some("tofu_init_failed")));
}

#[test]
fn converge_environment_vars_fixture_supports_transient_environment_values() {
    let repo = copy_fixture_repo("converge-environment-vars");

    let converge = with_local_first_env_disabled(|| {
        execute(
            &EngineRequest {
                operation: EngineOperation::Converge,
                target: Some(EnvironmentTarget {
                    environment: "pr-42".to_string(),
                }),
                selection: WorkspaceSelection::default(),
                wait_for: None,
            },
            repo.path(),
        )
    })
    .expect("transient converge should succeed");

    assert_eq!(converge.result.kind, OperationResultKind::Succeeded);

    let direct_outputs = run_tofu_output_json_for_env(repo.path(), "apps/web/infra", "pr-42");
    assert_eq!(
        direct_outputs["environment_descriptor"]["value"],
        json!("pr-42:transient")
    );

    let outputs = execute(
        &EngineRequest {
            operation: EngineOperation::Outputs,
            target: Some(EnvironmentTarget {
                environment: "pr-42".to_string(),
            }),
            selection: WorkspaceSelection {
                workspaces: vec!["apps/web/infra".to_string()],
            },
            wait_for: None,
        },
        repo.path(),
    )
    .expect("outputs should read transient converge state");

    assert_eq!(
        outputs.outputs["environment_descriptor"].value,
        json!("pr-42:transient")
    );
}

#[test]
fn status_after_transient_converge_reports_present_materialization() {
    let repo = copy_fixture_repo("converge-environment-vars");

    with_local_first_env_disabled(|| {
        execute(
            &EngineRequest {
                operation: EngineOperation::Converge,
                target: Some(EnvironmentTarget {
                    environment: "pr-42".to_string(),
                }),
                selection: WorkspaceSelection::default(),
                wait_for: None,
            },
            repo.path(),
        )
    })
    .expect("transient converge should succeed");

    let response = execute(
        &EngineRequest {
            operation: EngineOperation::Status,
            target: Some(EnvironmentTarget {
                environment: "pr-42".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        },
        repo.path(),
    )
    .expect("status should succeed after converge");

    assert_eq!(
        response
            .environment
            .as_ref()
            .and_then(|environment| environment.materialization.as_deref()),
        Some("present")
    );
    assert!(response
        .workspaces
        .iter()
        .all(|workspace| workspace.materialization.as_deref() == Some("present")));
}

#[test]
fn wait_succeeds_for_acceptable_after_transient_converge() {
    let repo = copy_fixture_repo("converge-environment-vars");

    with_local_first_env_disabled(|| {
        execute(
            &EngineRequest {
                operation: EngineOperation::Converge,
                target: Some(EnvironmentTarget {
                    environment: "pr-42".to_string(),
                }),
                selection: WorkspaceSelection::default(),
                wait_for: None,
            },
            repo.path(),
        )
    })
    .expect("transient converge should succeed");

    let response = execute(
        &EngineRequest {
            operation: EngineOperation::Wait,
            target: Some(EnvironmentTarget {
                environment: "pr-42".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: Some("acceptable".to_string()),
        },
        repo.path(),
    )
    .expect("wait should succeed for acceptable");

    assert_eq!(response.result.kind, OperationResultKind::Succeeded);
    assert!(response
        .result
        .summary
        .contains("condition 'acceptable' met"));
}

#[test]
fn wait_times_out_when_condition_is_not_met() {
    let repo = copy_fixture_repo("outputs-minimal-single");
    let _guard = WAIT_ENV_LOCK.lock().expect("wait env lock should succeed");
    env::set_var("YAFFLE_WAIT_TIMEOUT_MS", "50");
    env::set_var("YAFFLE_WAIT_POLL_MS", "10");

    let response = execute(
        &EngineRequest {
            operation: EngineOperation::Wait,
            target: Some(EnvironmentTarget {
                environment: "main".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: Some("acceptable".to_string()),
        },
        repo.path(),
    )
    .expect("wait should return a blocked response on timeout");

    env::remove_var("YAFFLE_WAIT_TIMEOUT_MS");
    env::remove_var("YAFFLE_WAIT_POLL_MS");

    assert_eq!(response.result.kind, OperationResultKind::Blocked);
    assert!(response
        .result
        .summary
        .contains("condition 'acceptable' not met"));
}

#[test]
fn destroy_after_converge_clears_materialization() {
    let repo = copy_fixture_repo("converge-environment-vars");

    with_local_first_env_disabled(|| {
        execute(
            &EngineRequest {
                operation: EngineOperation::Converge,
                target: Some(EnvironmentTarget {
                    environment: "pr-42".to_string(),
                }),
                selection: WorkspaceSelection::default(),
                wait_for: None,
            },
            repo.path(),
        )
    })
    .expect("transient converge should succeed");

    let destroy = execute(
        &EngineRequest {
            operation: EngineOperation::Destroy,
            target: Some(EnvironmentTarget {
                environment: "pr-42".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        },
        repo.path(),
    )
    .expect("destroy should succeed");

    assert_eq!(destroy.result.kind, OperationResultKind::Succeeded);
    assert!(destroy.result.summary.contains("destroyed 2 workspace(s)"));

    let status = execute(
        &EngineRequest {
            operation: EngineOperation::Status,
            target: Some(EnvironmentTarget {
                environment: "pr-42".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        },
        repo.path(),
    )
    .expect("status should succeed after destroy");

    assert_eq!(
        status
            .environment
            .as_ref()
            .and_then(|environment| environment.materialization.as_deref()),
        Some("absent")
    );
}

#[test]
fn converge_activation_webhook_fixture_dispatches_and_settles_activation() {
    let _guard = WAIT_ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let repo = copy_fixture_repo("converge-activation-webhook");
    write_fixture_git_remote(repo.path());

    let backend_listener = TcpListener::bind("127.0.0.1:0").expect("backend listener should bind");
    let backend_addr = backend_listener
        .local_addr()
        .expect("backend listener should have address");
    let hook_listener = TcpListener::bind("127.0.0.1:0").expect("hook listener should bind");
    let hook_addr = hook_listener
        .local_addr()
        .expect("hook listener should have address");

    let config_path = repo.path().join("yaffle.toml");
    let config = fs::read_to_string(&config_path).expect("fixture config should read");
    fs::write(
        &config_path,
        config
            .replace(
                "http://127.0.0.1:9999/hooks/preview-ready",
                &format!("http://{}/hooks/preview-ready", hook_addr),
            )
            .replace(
                "http://127.0.0.1:9999/hooks/preview-smoke",
                &format!("http://{}/hooks/preview-smoke", hook_addr),
            ),
    )
    .expect("fixture config should rewrite hook url");

    let lifecycle_state = Arc::new(StdMutex::new(FakeLifecycleState::default()));
    let backend_stop = Arc::new(AtomicBool::new(false));
    let hook_stop = Arc::new(AtomicBool::new(false));
    let backend_thread = spawn_fake_lifecycle_backend(
        backend_listener,
        backend_addr,
        lifecycle_state.clone(),
        backend_stop.clone(),
        true,
    );
    let hook_thread = spawn_activation_receiver(hook_listener, hook_stop.clone());

    let previous_module_api_host = env::var_os("YAFFLE_MODULE_API_HOST");
    env::set_var("YAFFLE_MODULE_API_HOST", format!("http://{backend_addr}"));

    let converge = execute(
        &EngineRequest {
            operation: EngineOperation::Converge,
            target: Some(EnvironmentTarget {
                environment: "main".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        },
        repo.path(),
    )
    .expect("converge should succeed with activation webhook");

    restore_env_var("YAFFLE_MODULE_API_HOST", previous_module_api_host);
    backend_stop.store(true, Ordering::SeqCst);
    hook_stop.store(true, Ordering::SeqCst);
    wake_listener(backend_addr);
    wake_listener(hook_addr);
    backend_thread.join().expect("backend thread should finish");
    hook_thread.join().expect("hook thread should finish");

    assert_eq!(converge.result.kind, OperationResultKind::Succeeded);
    assert!(converge
        .result
        .summary
        .contains("lifecycle settled: 2 item(s)"));

    let status = execute(
        &EngineRequest {
            operation: EngineOperation::Status,
            target: Some(EnvironmentTarget {
                environment: "main".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        },
        repo.path(),
    )
    .expect("status should load lifecycle state");

    let conditions = status
        .environment
        .as_ref()
        .expect("status should include environment")
        .conditions
        .iter()
        .filter_map(|value| {
            Some((
                value.get("name")?.as_str()?.to_string(),
                value.get("met")?.as_bool()?,
            ))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(conditions.get("activation_settled"), Some(&true));
    assert_eq!(conditions.get("verification_settled"), Some(&true));
    assert_eq!(conditions.get("usable"), Some(&true));
    assert_eq!(conditions.get("acceptable"), Some(&true));
}

#[test]
fn converge_blocks_before_infra_when_environment_governance_requires_cloud() {
    let _guard = WAIT_ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let repo = copy_fixture_repo("converge-activation-webhook");
    write_fixture_git_remote(repo.path());

    let backend_listener = TcpListener::bind("127.0.0.1:0").expect("backend listener should bind");
    let backend_addr = backend_listener
        .local_addr()
        .expect("backend listener should have address");
    let lifecycle_state = Arc::new(StdMutex::new(FakeLifecycleState::default()));
    let backend_stop = Arc::new(AtomicBool::new(false));
    let backend_thread = spawn_fake_lifecycle_backend(
        backend_listener,
        backend_addr,
        lifecycle_state,
        backend_stop.clone(),
        false,
    );

    let previous_module_api_host = env::var_os("YAFFLE_MODULE_API_HOST");
    env::set_var("YAFFLE_MODULE_API_HOST", format!("http://{backend_addr}"));

    let error = execute(
        &EngineRequest {
            operation: EngineOperation::Converge,
            target: Some(EnvironmentTarget {
                environment: "main".to_string(),
            }),
            selection: WorkspaceSelection::default(),
            wait_for: None,
        },
        repo.path(),
    )
    .expect_err("converge should be blocked by environment governance");

    restore_env_var("YAFFLE_MODULE_API_HOST", previous_module_api_host);
    backend_stop.store(true, Ordering::SeqCst);
    wake_listener(backend_addr);
    backend_thread.join().expect("backend thread should finish");

    assert_eq!(error.error.code, "environment_governance_blocked");
    assert!(error.error.message.contains("requires central execution"));
    assert!(!repo
        .path()
        .join(".yaffle/state/main/infra/single/terraform.tfstate")
        .exists());
}

struct FakeLifecycleState {
    activation_state: String,
    activation_summary: Option<String>,
    verification_state: String,
    verification_summary: Option<String>,
    created_item_count: usize,
}

impl Default for FakeLifecycleState {
    fn default() -> Self {
        Self {
            activation_state: "pending".to_string(),
            activation_summary: None,
            verification_state: "pending".to_string(),
            verification_summary: None,
            created_item_count: 0,
        }
    }
}

fn spawn_fake_lifecycle_backend(
    listener: TcpListener,
    backend_addr: std::net::SocketAddr,
    lifecycle_state: Arc<StdMutex<FakeLifecycleState>>,
    stop: Arc<AtomicBool>,
    admission_allowed: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        listener
            .set_nonblocking(true)
            .expect("backend listener should be nonblocking");
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let request = read_http_request(&mut stream);
                    if request.starts_with("POST /api/sessions/anonymous HTTP/1.1") {
                        write_http_json(
                            &mut stream,
                            201,
                            json!({
                                "data": {
                                    "principalType": "anonymous_session",
                                    "principalId": "principal-test",
                                    "sessionId": "session-test",
                                    "token": "principal-token-test",
                                    "issuedAt": "2026-05-04T00:00:00Z",
                                    "expiresAt": "2030-05-04T00:00:00Z"
                                }
                            }),
                        );
                    } else if request.starts_with("PUT /api/output-modules HTTP/1.1") {
                        write_http_json(
                            &mut stream,
                            201,
                            json!({
                                "data": {
                                    "id": "module-1",
                                    "repoBindingId": "binding-1",
                                    "workspacePath": "infra/single",
                                    "environmentName": "main",
                                    "versionSerial": 1,
                                    "version": "1.0.1",
                                    "createdAt": "2026-05-04T00:00:00Z"
                                }
                            }),
                        );
                    } else if request.starts_with("POST /api/lifecycle/runs HTTP/1.1") {
                        write_http_json(
                            &mut stream,
                            201,
                            json!({
                                "data": {
                                    "id": "run-1",
                                    "repoBindingId": "binding-1",
                                    "environmentName": "main",
                                    "executionMode": "local",
                                    "status": "running",
                                    "startedAt": "2026-05-04T00:00:00Z"
                                }
                            }),
                        );
                    } else if request.starts_with("POST /api/lifecycle/admission HTTP/1.1") {
                        write_http_json(
                            &mut stream,
                            200,
                            json!({
                                "data": {
                                    "allowed": admission_allowed,
                                    "reason": if admission_allowed { serde_json::Value::Null } else { json!("Environment 'main' requires central execution for governed runs, but this run is 'local'.") }
                                }
                            }),
                        );
                    } else if request.starts_with("POST /api/lifecycle/items HTTP/1.1") {
                        let mut state = lifecycle_state.lock().expect("state lock should work");
                        state.created_item_count += 1;
                        let (item_id, token, key, phase, scopes) = if state.created_item_count == 1
                        {
                            state.activation_state = "pending".to_string();
                            (
                                "item-1",
                                "token-1",
                                "preview-ready",
                                "activation",
                                json!(["usable", "acceptable"]),
                            )
                        } else {
                            state.verification_state = "pending".to_string();
                            (
                                "item-2",
                                "token-2",
                                "preview-smoke",
                                "verification",
                                json!(["acceptable"]),
                            )
                        };
                        write_http_json(
                            &mut stream,
                            201,
                            json!({
                                "data": {
                                    "id": item_id,
                                    "state": "pending",
                                    "onCompletionUrl": format!("http://{backend_addr}/api/lifecycle/completions/{token}"),
                                    "_key": key,
                                    "_phase": phase,
                                    "_scopes": scopes
                                }
                            }),
                        );
                    } else if request
                        .starts_with("POST /api/lifecycle/completions/token-1 HTTP/1.1")
                        || request.starts_with("POST /api/lifecycle/completions/token-2 HTTP/1.1")
                    {
                        let body = extract_http_body(&request);
                        let payload: serde_json::Value =
                            serde_json::from_str(&body).expect("callback payload should parse");
                        let mut state = lifecycle_state.lock().expect("state lock should work");
                        let is_activation = request.contains("token-1");
                        if is_activation {
                            state.activation_state =
                                payload["status"].as_str().unwrap_or("failed").to_string();
                            state.activation_summary =
                                payload["summary"].as_str().map(ToOwned::to_owned);
                        } else {
                            state.verification_state =
                                payload["status"].as_str().unwrap_or("failed").to_string();
                            state.verification_summary =
                                payload["summary"].as_str().map(ToOwned::to_owned);
                        }
                        write_http_json(
                            &mut stream,
                            200,
                            json!({ "data": { "id": if is_activation { "item-1" } else { "item-2" }, "state": if is_activation { state.activation_state.clone() } else { state.verification_state.clone() } } }),
                        );
                    } else if request.starts_with("GET /api/lifecycle/items/item-1 HTTP/1.1")
                        || request.starts_with("GET /api/lifecycle/items/item-2 HTTP/1.1")
                    {
                        let state = lifecycle_state.lock().expect("state lock should work");
                        let is_activation = request.contains("item-1");
                        write_http_json(
                            &mut stream,
                            200,
                            json!({
                                "data": {
                                    "id": if is_activation { "item-1" } else { "item-2" },
                                    "workspacePath": "infra/single",
                                    "key": if is_activation { "preview-ready" } else { "preview-smoke" },
                                    "phase": if is_activation { "activation" } else { "verification" },
                                    "state": if is_activation { state.activation_state.as_str() } else { state.verification_state.as_str() },
                                    "failurePolicy": "failed",
                                    "scopes": if is_activation { json!(["usable", "acceptable"]) } else { json!(["acceptable"]) },
                                    "summary": if is_activation { json!(state.activation_summary) } else { json!(state.verification_summary) },
                                    "reason": null,
                                    "metadata": {},
                                    "startedAt": "2026-05-04T00:00:00Z",
                                    "finishedAt": if (if is_activation { state.activation_state.as_str() } else { state.verification_state.as_str() }) == "succeeded" { Some("2026-05-04T00:00:01Z") } else { None }
                                }
                            }),
                        );
                    } else if request.contains("GET /api/lifecycle/state?") {
                        let state = lifecycle_state.lock().expect("state lock should work");
                        write_http_json(
                            &mut stream,
                            200,
                            json!({
                                "data": {
                                    "run": {
                                        "id": "run-1",
                                        "status": if state.activation_state == "succeeded" && state.verification_state == "succeeded" { "succeeded" } else { "running" },
                                        "executionMode": "local",
                                        "startedAt": "2026-05-04T00:00:00Z",
                                        "finishedAt": if state.activation_state == "succeeded" && state.verification_state == "succeeded" { Some("2026-05-04T00:00:01Z") } else { None }
                                    },
                                    "items": [
                                      {
                                        "id": "item-1",
                                        "workspacePath": "infra/single",
                                        "key": "preview-ready",
                                        "phase": "activation",
                                        "state": state.activation_state,
                                        "failurePolicy": "failed",
                                        "scopes": ["usable", "acceptable"],
                                        "summary": state.activation_summary,
                                        "reason": null,
                                        "metadata": {},
                                        "startedAt": "2026-05-04T00:00:00Z",
                                        "finishedAt": if state.activation_state == "succeeded" { Some("2026-05-04T00:00:01Z") } else { None }
                                      },
                                      {
                                        "id": "item-2",
                                        "workspacePath": "infra/single",
                                        "key": "preview-smoke",
                                        "phase": "verification",
                                        "state": state.verification_state,
                                        "failurePolicy": "failed",
                                        "scopes": ["acceptable"],
                                        "summary": state.verification_summary,
                                        "reason": null,
                                        "metadata": {},
                                        "startedAt": "2026-05-04T00:00:00Z",
                                        "finishedAt": if state.verification_state == "succeeded" { Some("2026-05-04T00:00:01Z") } else { None }
                                      }
                                    ]
                                }
                            }),
                        );
                    } else {
                        write_http_json(
                            &mut stream,
                            404,
                            json!({ "error": { "code": "NOT_FOUND", "message": "not found" } }),
                        );
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("backend accept failed: {error}"),
            }
        }
    })
}

fn spawn_activation_receiver(
    listener: TcpListener,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        listener
            .set_nonblocking(true)
            .expect("hook listener should be nonblocking");
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client should build");

        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let request = read_http_request(&mut stream);
                    if request.starts_with("POST /hooks/preview-ready HTTP/1.1") {
                        let body = extract_http_body(&request);
                        let payload: serde_json::Value =
                            serde_json::from_str(&body).expect("webhook payload should parse");
                        assert_eq!(payload["workspace_path"], json!("infra/single"));
                        assert_eq!(
                            payload["outputs"]["service_name"]["value"],
                            json!("single-service")
                        );
                        let callback = payload["on_completion"]
                            .as_str()
                            .expect("callback url should exist");
                        client
                            .post(callback)
                            .json(&json!({
                                "status": "succeeded",
                                "summary": "preview-ready completed"
                            }))
                            .send()
                            .expect("callback should succeed");
                        write_http_json(&mut stream, 202, json!({ "ok": true }));
                    } else if request.starts_with("POST /hooks/preview-smoke HTTP/1.1") {
                        let body = extract_http_body(&request);
                        let payload: serde_json::Value =
                            serde_json::from_str(&body).expect("verification payload should parse");
                        assert_eq!(payload["phase"], json!("verification"));
                        let callback = payload["on_completion"]
                            .as_str()
                            .expect("verification callback url should exist");
                        client
                            .post(callback)
                            .json(&json!({
                                "status": "succeeded",
                                "summary": "preview-smoke completed"
                            }))
                            .send()
                            .expect("verification callback should succeed");
                        write_http_json(&mut stream, 202, json!({ "ok": true }));
                    } else {
                        write_http_json(&mut stream, 404, json!({ "error": "not found" }));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("hook accept failed: {error}"),
            }
        }
    })
}

fn read_http_request(stream: &mut std::net::TcpStream) -> String {
    stream
        .set_nonblocking(false)
        .expect("request stream should be blocking");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("request stream should have a read timeout");
    let mut buffer = [0_u8; 16_384];
    let bytes_read = stream.read(&mut buffer).expect("request should read");
    String::from_utf8_lossy(&buffer[..bytes_read]).to_string()
}

fn extract_http_body(request: &str) -> String {
    request
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or_default()
        .to_string()
}

fn write_http_json(stream: &mut std::net::TcpStream, status: u16, body: serde_json::Value) {
    let status_text = match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        404 => "Not Found",
        _ => "OK",
    };
    let body_text = body.to_string();
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body_text.len(),
        body_text,
    );
    stream
        .write_all(response.as_bytes())
        .expect("response should write");
}

fn write_fixture_git_remote(repo_root: &std::path::Path) {
    let git_dir = repo_root.join(".git");
    fs::create_dir_all(&git_dir).expect("git dir should exist");
    fs::write(
        git_dir.join("config"),
        "[remote \"origin\"]\n  url = https://github.com/test-org/fixture.git\n",
    )
    .expect("git config should write");
}

fn restore_env_var(name: &str, previous: Option<std::ffi::OsString>) {
    if let Some(previous) = previous {
        env::set_var(name, previous);
    } else {
        env::remove_var(name);
    }
}

fn wake_listener(address: std::net::SocketAddr) {
    let _ = std::net::TcpStream::connect(address);
}

fn with_local_first_env_disabled<T>(callback: impl FnOnce() -> T) -> T {
    let _guard = WAIT_ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let previous_module_api_host = env::var_os("YAFFLE_MODULE_API_HOST");
    env::remove_var("YAFFLE_MODULE_API_HOST");
    let result = callback();
    restore_env_var("YAFFLE_MODULE_API_HOST", previous_module_api_host);
    result
}
