terraform {
  required_version = ">= 1.8.0"
}

resource "terraform_data" "fails_during_destroy" {
  input = "retained"

  provisioner "local-exec" {
    when    = destroy
    command = "exit 1"
  }
}

resource "terraform_data" "deleted_before_failure" {
  input      = "removed"
  depends_on = [terraform_data.fails_during_destroy]
}
