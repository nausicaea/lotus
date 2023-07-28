#!/bin/sh
# Builds the project in release mode into a statically linked binary (to be self-contained)

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )
exec docker build -t lotus:latest $SCRIPT_DIR
