variable "environment" {
  type = string
}

variable "environment_kind" {
  type = string
}

locals {
  environment_descriptor = "${var.environment}:${var.environment_kind}"
}

output "environment_descriptor" {
  value = local.environment_descriptor
}
