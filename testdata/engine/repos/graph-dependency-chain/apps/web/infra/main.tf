module "shared" {
  source = "yaffle.dev/test-org--fixture/infra--shared/yaffle"
}

module "api" {
  source = "yaffle.dev/test-org--fixture/apps--api--infra/yaffle"
}
