use std::collections::BTreeMap;

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use serde_json::Value;

pub const CONTRACT_VERSION: u16 = 1;
pub const SHARED_OUTPUT_SNAPSHOT_CONTRACT_VERSION: u16 = 1;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct EnvironmentName(String);

impl EnvironmentName {
    pub fn new(name: String) -> Result<Self, String> {
        if name.trim().is_empty() {
            return Err("environment name must not be empty".to_string());
        }

        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for EnvironmentName {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for EnvironmentName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::try_from(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum EnvironmentIdentity {
    TransientManaged { name: EnvironmentName },
    NamedManaged { name: EnvironmentName },
    NamedExternal { name: EnvironmentName },
    StaticExternal,
}

impl EnvironmentIdentity {
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::TransientManaged { name }
            | Self::NamedManaged { name }
            | Self::NamedExternal { name } => Some(name.as_str()),
            Self::StaticExternal => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SharedOutputProducer {
    pub organization_id: String,
    pub organization: String,
    pub repository_id: String,
    pub repository: String,
    pub workspace: String,
    pub environment: EnvironmentIdentity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitSourceRevision {
    pub vcs: SourceControl,
    pub commit_sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceControl {
    Git,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct StateVersionIdentity(String);

impl StateVersionIdentity {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for StateVersionIdentity {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let Some(identifier) = value.strip_prefix("statev_") else {
            return Err("state identity must be an opaque statev_ identifier".to_string());
        };
        if identifier.is_empty()
            || !identifier
                .chars()
                .all(|character| character.is_ascii_alphanumeric())
        {
            return Err("state identity must be an opaque statev_ identifier".to_string());
        }

        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for StateVersionIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::try_from(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct StateSerial(u64);

impl StateSerial {
    pub fn new(serial: u64) -> Result<Self, String> {
        if serial > MAX_SAFE_INTEGER {
            return Err("state serial must be a non-negative safe integer".to_string());
        }

        Ok(Self(serial))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for StateSerial {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerraformStateIdentity {
    /// Opaque Yaffle identity. Never expose a backend URL or state key here.
    pub identity: StateVersionIdentity,
    pub serial: StateSerial,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct PublicationVersion(u64);

impl PublicationVersion {
    pub fn new(version: u64) -> Result<Self, String> {
        if version == 0 || version > MAX_SAFE_INTEGER {
            return Err("publication version must be a positive safe integer".to_string());
        }

        Ok(Self(version))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for PublicationVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedOutputSnapshotContractVersion;

impl Serialize for SharedOutputSnapshotContractVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u16(SHARED_OUTPUT_SNAPSHOT_CONTRACT_VERSION)
    }
}

impl<'de> Deserialize<'de> for SharedOutputSnapshotContractVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let version = u16::deserialize(deserializer)?;
        if version != SHARED_OUTPUT_SNAPSHOT_CONTRACT_VERSION {
            return Err(D::Error::custom(format!(
                "expected shared output snapshot contract version {}, got {version}",
                SHARED_OUTPUT_SNAPSHOT_CONTRACT_VERSION
            )));
        }

        Ok(Self)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SharedOutputValue {
    value: Value,
    sensitive: bool,
}

impl SharedOutputValue {
    pub fn visible(value: Value) -> Self {
        Self {
            value,
            sensitive: false,
        }
    }

    pub fn redacted() -> Self {
        Self {
            value: Value::Null,
            sensitive: true,
        }
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn is_sensitive(&self) -> bool {
        self.sensitive
    }
}

impl<'de> Deserialize<'de> for SharedOutputValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireValue {
            value: Value,
            sensitive: bool,
        }

        let wire = WireValue::deserialize(deserializer)?;
        if wire.sensitive {
            if !wire.value.is_null() {
                return Err(D::Error::custom(
                    "sensitive shared output values must be redacted",
                ));
            }
            return Ok(Self::redacted());
        }

        Ok(Self::visible(wire.value))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SharedOutputSnapshotV1 {
    pub contract_version: SharedOutputSnapshotContractVersion,
    pub snapshot_id: String,
    pub publication_version: PublicationVersion,
    pub producer: SharedOutputProducer,
    pub source_revision: GitSourceRevision,
    pub state: TerraformStateIdentity,
    pub published_at: String,
    pub values: BTreeMap<String, SharedOutputValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvironmentTarget {
    pub environment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkspaceSelection {
    pub workspaces: Vec<String>,
}

impl WorkspaceSelection {
    pub fn is_empty(&self) -> bool {
        self.workspaces.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EngineOperation {
    Converge,
    Destroy,
    Status,
    Wait,
    Outputs,
    Graph,
    Doctor,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationResultKind {
    Succeeded,
    Degraded,
    Blocked,
    Failed,
    Partial,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationResult {
    pub kind: OperationResultKind,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiagnosticMessage {
    pub level: DiagnosticLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TerraformOutput {
    pub value: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sensitive: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EngineMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnvironmentSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<Value>,
    pub conditions: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub materialization: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freshness: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceSnapshot {
    pub workspace_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub materialization: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freshness: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EngineResponse {
    pub contract_version: u16,
    pub operation: EngineOperation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<EnvironmentTarget>,
    pub selection: WorkspaceSelection,
    pub result: OperationResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<EnvironmentSnapshot>,
    pub workspaces: Vec<WorkspaceSnapshot>,
    pub outputs: BTreeMap<String, TerraformOutput>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub workspace_outputs: BTreeMap<String, BTreeMap<String, TerraformOutput>>,
    pub diagnostics: Vec<DiagnosticMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<EngineMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ErrorPayload {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EngineError {
    pub contract_version: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<EngineOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<EnvironmentTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<WorkspaceSelection>,
    pub error: ErrorPayload,
}

#[cfg(test)]
mod tests {
    use super::{SharedOutputSnapshotContractVersion, SharedOutputSnapshotV1};

    #[test]
    fn shared_output_snapshot_matches_cross_language_fixture() {
        let fixture = include_str!("../../../testdata/contracts/shared-output-snapshot-v1.json");
        let snapshot: SharedOutputSnapshotV1 = serde_json::from_str(fixture).unwrap();

        assert_eq!(
            snapshot.contract_version,
            SharedOutputSnapshotContractVersion
        );
        assert_eq!(snapshot.snapshot_id, "sos_01JZXNS2Q6MEW4NZ4M9T8J2R7K");
        assert_eq!(snapshot.publication_version.get(), 7);
        assert_eq!(snapshot.producer.organization, "acme");
        assert_eq!(snapshot.producer.environment.name(), Some("review-42"));
        assert_eq!(snapshot.source_revision.commit_sha.len(), 40);
        assert_eq!(
            snapshot.state.identity.as_str(),
            "statev_01JZXNT7PND73G9ESQZT15E0H8"
        );
        assert_eq!(snapshot.state.serial.get(), 42);
        assert!(snapshot.values["databasePassword"].is_sensitive());
        assert!(snapshot.values["databasePassword"].value().is_null());
    }

    #[test]
    fn shared_output_snapshot_rejects_other_contract_versions() {
        let fixture = include_str!("../../../testdata/contracts/shared-output-snapshot-v1.json");
        let mut value: serde_json::Value = serde_json::from_str(fixture).unwrap();
        value["contractVersion"] = serde_json::json!(2);

        let error = serde_json::from_value::<SharedOutputSnapshotV1>(value).unwrap_err();

        assert!(error
            .to_string()
            .contains("expected shared output snapshot contract version 1, got 2"));
    }

    #[test]
    fn shared_output_snapshot_rejects_empty_environment_names() {
        let fixture = include_str!("../../../testdata/contracts/shared-output-snapshot-v1.json");
        let mut value: serde_json::Value = serde_json::from_str(fixture).unwrap();
        value["producer"]["environment"]["name"] = serde_json::json!(" ");

        let error = serde_json::from_value::<SharedOutputSnapshotV1>(value).unwrap_err();

        assert!(error
            .to_string()
            .contains("environment name must not be empty"));
    }

    #[test]
    fn shared_output_snapshot_rejects_unredacted_sensitive_values() {
        let fixture = include_str!("../../../testdata/contracts/shared-output-snapshot-v1.json");
        let mut value: serde_json::Value = serde_json::from_str(fixture).unwrap();
        value["values"]["databasePassword"]["value"] = serde_json::json!("secret");

        let error = serde_json::from_value::<SharedOutputSnapshotV1>(value).unwrap_err();

        assert!(error
            .to_string()
            .contains("sensitive shared output values must be redacted"));
    }

    #[test]
    fn shared_output_snapshot_rejects_physical_state_locations() {
        let fixture = include_str!("../../../testdata/contracts/shared-output-snapshot-v1.json");
        let mut value: serde_json::Value = serde_json::from_str(fixture).unwrap();
        value["state"]["identity"] = serde_json::json!("s3://state-bucket/key");

        let error = serde_json::from_value::<SharedOutputSnapshotV1>(value).unwrap_err();

        assert!(error
            .to_string()
            .contains("state identity must be an opaque statev_ identifier"));
    }

    #[test]
    fn shared_output_snapshot_rejects_integers_unsafe_for_javascript() {
        let fixture = include_str!("../../../testdata/contracts/shared-output-snapshot-v1.json");
        let mut value: serde_json::Value = serde_json::from_str(fixture).unwrap();
        value["publicationVersion"] = serde_json::json!(9_007_199_254_740_992_u64);

        let error = serde_json::from_value::<SharedOutputSnapshotV1>(value).unwrap_err();

        assert!(error
            .to_string()
            .contains("publication version must be a positive safe integer"));
    }
}
