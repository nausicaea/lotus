#!/bin/sh
# Builds the project in release mode into a statically linked binary (to be self-contained)

exec docker run -v $PWD:/volume --rm -t docker.io/clux/muslrust:stable cargo build --release
