#!/usr/bin/env bash

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 <num_processes>"
  exit 1
fi

NUM_PROCS=$1
TEST_SCRIPT_PATH="$(realpath ./resource_eater.sh)"  # make sure it's executable
YAPM_BINARY="./target/release/yapm"  # adjust if not in same directory

if [[ ! -x "$TEST_SCRIPT_PATH" ]]; then
  echo "Test script $TEST_SCRIPT_PATH is not executable."
  exit 1
fi

if [[ ! -x "$YAPM_BINARY" ]]; then
  echo "YAPM binary not found at $YAPM_BINARY"
  exit 1
fi

echo "Spawning $NUM_PROCS processes using YAPM..."
START=$(date +%s.%N)

for i in $(seq 1 "$NUM_PROCS"); do
  name="test_$i"
  "$YAPM_BINARY" process start --name "$name" --command "$TEST_SCRIPT_PATH" >/dev/null 2>&1 &
  sleep 0.05
done


wait  # Wait for all backgrounded YAPM spawns to complete

END=$(date +%s.%N)
TOTAL_TIME=$(echo "$END - $START" | bc -l)
AVG_TIME=$(echo "$TOTAL_TIME / $NUM_PROCS" | bc -l)

printf "Total time: %.3f seconds\n" "$TOTAL_TIME"
printf "Average time per process: %.3f seconds\n" "$AVG_TIME"

