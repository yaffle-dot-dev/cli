# CLI Process Contract

Yaffle v0.1 uses contract version `1` for engine success and error documents.

## JSON output

Scriptable engine commands accept `--json`. They write exactly one JSON document to stdout and do
not mix notices or progress rendering into that stream.

Successful and completed operation reports contain:

```json
{
  "contract_version": 1,
  "operation": "doctor",
  "selection": { "workspaces": [] },
  "result": { "kind": "succeeded", "summary": "doctor passed" },
  "workspaces": [],
  "outputs": {},
  "diagnostics": []
}
```

Failures that prevent an operation report use:

```json
{
  "contract_version": 1,
  "operation": "graph",
  "selection": { "workspaces": [] },
  "error": { "code": "config_not_found", "message": "configuration was not found" }
}
```

Fields may be added compatibly within contract version 1. Existing fields, enum values, and meanings
will not be removed or changed without a contract-version increment.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | The command completed with a `succeeded`, `degraded`, or `partial` result. |
| `1` | The operation failed, was blocked, or could not produce an operation report. |
| `2` | Command-line arguments were invalid. |

JSON and human-readable modes use the same exit status for the same operation result.
