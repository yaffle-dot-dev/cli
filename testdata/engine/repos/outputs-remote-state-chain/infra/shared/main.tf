terraform {
  required_version = ">= 1.8.0"
}

locals {
  domain = "shared.internal"
  ports = {
    http  = 8080
    https = 8443
  }
  features = ["auth", "cdn", "metrics"]
}

output "domain" {
  value = local.domain
}

output "ports" {
  value = local.ports
}

output "features" {
  value = local.features
}
