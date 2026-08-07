terraform {
  required_version = ">= 1.8.0"
}

resource "terraform_data" "created_before_failure" {
  input = "durable"
}

resource "terraform_data" "fails_after_create" {
  depends_on = [terraform_data.created_before_failure]

  provisioner "local-exec" {
    command = "exit 1"
  }
}
