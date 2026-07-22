use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

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
