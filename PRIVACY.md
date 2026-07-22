# Privacy and Data Transfers

## Local commands

Local graph, doctor, converge, destroy, status, wait, and output operations read repository files and
run OpenTofu on your machine. They do not require a Yaffle account. OpenTofu and configured providers
may contact provider registries and infrastructure APIs according to your Terraform configuration.

## Yaffle Cloud commands

`yaffle cloud login` opens Yaffle's GitHub authentication flow and sends an OAuth authorization
request, PKCE challenge, loopback callback port, and random state value to Yaffle Cloud. After user
approval, Yaffle returns an account credential that the CLI stores locally. The CLI never sends the
PKCE verifier through the browser and does not use a shared client secret.

Hosted operations may send repository identity, Git revision, environment and workspace selection,
operation status, and non-secret diagnostics to Yaffle Cloud. Do not place secrets in workspace
names, output names, or command-line arguments. Terraform state and sensitive outputs require the
separate authenticated state/output contracts; the CLI must not include them in telemetry.

GitHub processes authentication data under GitHub's privacy terms. Yaffle infrastructure may process
service data in the United States. Contact `privacy@yaffle.dev` for privacy questions or data-access
and deletion requests.

Local credentials are stored in `~/.yaffle/auth/principal.json`; on Unix the directory and file are
restricted to modes `0700` and `0600`. Run `yaffle cloud logout` to remove the local credential.
