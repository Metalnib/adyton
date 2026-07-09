#!/bin/sh
# Binary-size budget gate (specification §8). Usage: size-gate.sh <binary> <max-bytes>
set -eu
bin=$1
max=$2
size=$(wc -c <"$bin" | tr -d ' ')
echo "binary size: ${size} bytes (budget ${max})"
[ "$size" -le "$max" ]
