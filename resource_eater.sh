#!/usr/bin/env bash

set -euo pipefail

END_TIME=$((SECONDS + 30))

while [[ $SECONDS -lt $END_TIME ]]; do
  echo "I'm alive, happy, and eating resources"
  echo "I'm alive, happy, and eating resources" 1>&2
  sleep 5
done

