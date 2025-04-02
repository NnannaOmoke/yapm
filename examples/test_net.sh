#!/usr/bin/env bash

# Test script to check network connection under seccomp rules

# Function to print error and exit
print_error() {
    echo "Error: $1" >&2
    exit 1
}

# Test 1: Attempt to make a network connection with curl
echo "Attempting to fetch a webpage with curl..." >&2
curl -s -o /dev/null https://google.com || print_error "curl failed with exit code $?"

# If we get here, the network connection worked
echo "Success: Network connection established with curl." >&2

exit 0
