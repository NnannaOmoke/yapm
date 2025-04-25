#!/usr/bin/env bash

# Test script to check IPC under seccomp rules

# Function to print error and exit
print_error() {
    echo "Error: $1" >&2
    exit 1
}

# Test: Attempt to create a named pipe and write to it
echo "Attempting to create and write to a named pipe..." >&2
FIFO_PATH=$(mktemp -u)
mkfifo "$FIFO_PATH" || print_error "Creating named pipe failed with exit code $?"
echo "Test message" > "$FIFO_PATH" &  # Write to the pipe in the background
read -t 1 < "$FIFO_PATH" || print_error "Writing to named pipe failed with exit code $?"
rm "$FIFO_PATH"

# If we get here, the operation succeeded
echo "Success: IPC operation (named pipe) completed." >&2

exit 0
