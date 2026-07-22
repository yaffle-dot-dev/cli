module "named" {
  source = "yaffle.dev/test-org--fixture/infra--named/yaffle"
}

output "named_environment" {
  value = module.named.environment_name
}
