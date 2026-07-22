terraform {
  required_version = ">= 1.8.0"
}

locals {
  service_name = "single-service"
  numbers      = [1, 2, 3]
  settings = {
    enabled = true
    tier    = "test"
  }
}

output "service_name" {
  value = local.service_name
}

output "numbers" {
  value = local.numbers
}

output "settings" {
  value = local.settings
}
