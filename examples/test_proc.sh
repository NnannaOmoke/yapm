#!/usr/bin/env bash

# Test script to check process creation under seccomp rules

# Function to print error and exit
print_error() {
    echo "Error: $1" >&2
    exit 1
}

# Test: Attempt to run an external command
sleep 8

echo "Attempting to run an external command..." >&2
/bin/echo "Hello from external echo" || print_error "Running external command failed with exit code $?"

# If we get here, the operation succeeded
echo "Success: External command executed." >&2

exit 0
