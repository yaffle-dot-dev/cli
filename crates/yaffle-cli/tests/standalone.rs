use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

#[test]
fn legacy_approval_diagnostics_preserve_graph_json_and_never_echo_selectors() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/engine/repos/outputs-minimal-single");
    let working_directory = TempDir::new().expect("temporary directory should exist");
    let workspace = working_directory.path().join("infra/single");
    std::fs::create_dir_all(&workspace).expect("workspace should exist");
    std::fs::copy(
        fixture.join("infra/single/main.tf"),
        workspace.join("main.tf"),
    )
    .expect("fixture should copy");
    let config = std::fs::read_to_string(fixture.join("yaffle.toml"))
        .expect("fixture config should be readable");
    let config_path = working_directory.path().join("yaffle.toml");
    std::fs::write(&config_path, &config).expect("config should write");
    let graph = || {
        Command::new(env!("CARGO_BIN_EXE_yaffle"))
            .args(["graph", "--env", "main", "--json"])
            .current_dir(working_directory.path())
            .output()
            .expect("graph should run")
    };
    let baseline = graph();
    assert!(baseline.status.success());
    assert!(baseline.stderr.is_empty());
    serde_json::from_slice::<serde_json::Value>(&baseline.stdout)
        .expect("stdout should contain exactly one JSON document");

    for legacy in [
        "[cloud]\napprovals = []",
        "[\"cloud\"]\n\"approvals\" = []",
        "[[cloud.approvals]]\nworkspaces = [\"infra/*\"]\nenvironments = [\"main\"]\napprovers = [\"github:user:private-reviewer-selector\", \"github:team:private-org/private-team\"]",
    ] {
        std::fs::write(&config_path, format!("{config}\n{legacy}\n"))
            .expect("legacy config should write");
        let output = graph();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, baseline.stdout);
        let diagnostic = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
        assert!(diagnostic.contains("cloud.approvals is deprecated"));
        assert!(diagnostic.contains("Settings > Review policies"));
        assert!(!diagnostic.contains("private-reviewer-selector"));
        assert!(!diagnostic.contains("private-org"));
        assert!(!diagnostic.contains("infra/*"));
    }

    std::fs::write(&config_path, format!("{config}\n# cloud.approvals = []\n"))
        .expect("comment config should write");
    let output = graph();
    assert!(output.status.success());
    assert_eq!(output.stdout, baseline.stdout);
    assert!(output.stderr.is_empty());
}

#[test]
fn no_subcommand_prints_long_help() {
    let working_directory = TempDir::new().expect("temporary directory should exist");
    let output = Command::new(env!("CARGO_BIN_EXE_yaffle"))
        .current_dir(working_directory.path())
        .output()
        .expect("yaffle should start");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert!(stdout.contains("Usage: yaffle [COMMAND]"));
}

#[test]
fn doctor_json_reports_an_operational_failure_exit() {
    let working_directory = TempDir::new().expect("temporary directory should exist");
    let output = Command::new(env!("CARGO_BIN_EXE_yaffle"))
        .args(["doctor", "--json"])
        .current_dir(working_directory.path())
        .output()
        .expect("yaffle doctor should run");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());

    let response: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should contain one JSON document");
    assert_eq!(response["contract_version"], 1);
    assert_eq!(response["operation"], "doctor");
    assert_eq!(response["result"]["kind"], "failed");
}

#[test]
fn graph_error_matches_the_version_one_golden_contract() {
    let working_directory = TempDir::new().expect("temporary directory should exist");
    let output = Command::new(env!("CARGO_BIN_EXE_yaffle"))
        .args(["graph", "--json"])
        .current_dir(working_directory.path())
        .output()
        .expect("yaffle graph should run");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());

    let mut response: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should contain one JSON document");
    response["error"]["message"] = serde_json::json!("<working-directory>");
    let golden: serde_json::Value = serde_json::from_str(include_str!(
        "../../../testdata/contracts/cli-engine-error-v1.json"
    ))
    .expect("golden contract should be valid JSON");

    assert_eq!(response, golden);
}

#[test]
fn graph_success_matches_the_version_one_golden_contract() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/engine/repos/outputs-minimal-single");
    let output = Command::new(env!("CARGO_BIN_EXE_yaffle"))
        .args(["graph", "--env", "main", "--json"])
        .current_dir(fixture)
        .output()
        .expect("yaffle graph should run");

    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let mut response: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should contain one JSON document");
    response["diagnostics"][0]["message"] = serde_json::json!("<config-path>");
    let golden: serde_json::Value = serde_json::from_str(include_str!(
        "../../../testdata/contracts/cli-engine-success-v1.json"
    ))
    .expect("golden contract should be valid JSON");

    assert_eq!(response, golden);
}
