#!/bin/bash

source ./lib.sh

MAX_RETRIES=5
LOG_DIR="/var/log"

my_func() {
    echo "hello"
}

function process() {
    echo "processing"
}
