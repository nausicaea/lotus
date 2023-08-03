#!/bin/sh

exec pre-commit try-repo --hook-stage pre-commit $(git remote get-url origin) lotus
