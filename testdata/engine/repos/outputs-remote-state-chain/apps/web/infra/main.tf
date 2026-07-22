terraform {
  required_version = ">= 1.8.0"
}

data "terraform_remote_state" "shared" {
  backend = "local"

  config = {
    path = "../../../.yaffle/state/main/infra/shared/terraform.tfstate"
  }
}

locals {
  base_url = "https://${data.terraform_remote_state.shared.outputs.domain}"
}

output "base_url" {
  value = local.base_url
}

output "https_port" {
  value = data.terraform_remote_state.shared.outputs.ports["https"]
}

output "feature_flags" {
  value = data.terraform_remote_state.shared.outputs.features
}
