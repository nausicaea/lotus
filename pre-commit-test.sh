#!/bin/sh

exec pre-commit try-repo --hook-stage pre-push $(git remote get-url origin) lotus
