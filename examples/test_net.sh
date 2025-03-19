#!/usr/bin/env bash

# Target URL for connectivity check
TARGET_URL="https://httpbin.org/get"

# Run curl inside a new network namespace
unshare -n ping google.com
