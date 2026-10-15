terraform {
  required_version = ">= 1.5"
}

resource "aws_instance" "web" {
  ami           = "ami-123"
  instance_type = "t3.micro"

  network_interface {
    device_index = 0
  }
}

variable "region" {
  default = "us-east-1"
}
