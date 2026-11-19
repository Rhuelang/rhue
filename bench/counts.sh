#!/usr/bin/env bash
# Instruction counts for every script in bench/, from callgrind.
#
#   bench/counts.sh            print them
#   bench/counts.sh --save     write them to bench/counts.json
#   bench/counts.sh --check    fail if any got more than 2% worse than counts.json
#
# Wall times drift too much between runs (and on shared CI runners) to gate on,
# instruction counts don't. Each count covers the whole process (engine setup,
# compiling and one run of the script), Linux only since it needs valgrind.
set -euo pipefail

cd "$(dirname "$0")/.."
mode="${1:-}"
limit="${RHUE_BENCH_LIMIT:-2}"

command -v valgrind >/dev/null || { echo "needs valgrind" >&2; exit 1; }
cargo build -q --release -p rhue --example bench
bin=target/release/examples/bench
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT

json="{"
worse=0
printf '%-12s %15s %15s %8s\n' script instructions saved change
for script in bench/*.rhue; do
  name=$(basename "$script" .rhue)
  valgrind --tool=callgrind --callgrind-out-file="$out/$name" -q "$bin" --runs 1 "$name" >/dev/null
  # the summary line is "summary: <Ir>"
  count=$(awk '/^summary:/ {print $2}' "$out/$name")
  saved=""
  change=""
  if [[ -f bench/counts.json ]]; then
    saved=$(grep -o "\"$name\": *[0-9]*" bench/counts.json | grep -o '[0-9]*$' || true)
  fi
  if [[ -n "$saved" ]]; then
    change=$(awk -v a="$count" -v b="$saved" 'BEGIN { printf "%+.2f%%", (a - b) * 100 / b }')
    if [[ "$mode" == "--check" ]] && awk -v a="$count" -v b="$saved" -v l="$limit" 'BEGIN { exit !((a - b) * 100 / b > l) }'; then
      worse=1
      change="$change !"
    fi
  fi
  printf '%-12s %15s %15s %8s\n' "$name" "$count" "${saved:--}" "$change"
  json+="\n  \"$name\": $count,"
done
json="${json%,}\n}"

if [[ "$mode" == "--save" ]]; then
  printf "$json\n" > bench/counts.json
  echo "wrote bench/counts.json"
fi
if [[ "$worse" == 1 ]]; then
  echo "some benchmarks got more than ${limit}% worse" >&2
  exit 1
fi
