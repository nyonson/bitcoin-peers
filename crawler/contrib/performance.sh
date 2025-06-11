#!/usr/bin/env bash

# Performance testing script for bitcoin-peers crawler.
# 
# Usage:
#   ./crawler/contrib/performance.sh [CONCURRENT_TASKS] [DURATION_SECONDS]
#
# Arguments:
#   CONCURRENT_TASKS  - Number of concurrent tasks (default: 8)
#   DURATION_SECONDS  - Test duration in seconds (default: 60)
#
# Environment Variables:
#   SEED_ADDRESS      - IP address of seed node (default: 127.0.0.1)
#   SEED_PORT         - Port of seed node (default: 8333)
#   LOG_LEVEL         - Log level for crawler (default: info)
#
# Requirements:
#   - GNU time command (for resource monitoring)
# 
# Metrics collected:
#   - Peers discovered (total count and rate per second)
#   - Peak memory usage (KB)
#   - CPU time (user and system)
# 
# Output streams:
#   - stdout: Performance metrics (key=value format)
#   - stderr: Crawler logs
# 
# Examples:
#   # Basic test with defaults (8 tasks, 60 seconds)
#   ./crawler/contrib/performance.sh
#
#   # Test with 16 concurrent tasks for 60 seconds
#   ./crawler/contrib/performance.sh 16
#
#   # Test with 32 concurrent tasks for 120 seconds
#   ./crawler/contrib/performance.sh 32 120
#
#   # Test with custom seed
#   SEED_ADDRESS=192.168.1.100 ./crawler/contrib/performance.sh 16 90
#
#   # Use output in scripts
#   eval $(./crawler/contrib/performance.sh)
#   echo "Discovered $peers peers at $rate/sec"

set -e

SEED_ADDRESS="${SEED_ADDRESS:-127.0.0.1}"
SEED_PORT="${SEED_PORT:-8333}"
LOG_LEVEL="${LOG_LEVEL:-info}"
CONCURRENT_TASKS="${1:-8}"
TEST_DURATION_SECONDS="${2:-60}"

# Build in release mode first, direct logs to stderr.
cargo build --example crawler --release -p bitcoin-peers-crawler 1>&2

start_time=$(date +%s)
temp_stats=$(mktemp)

echo "performance: raw output saved to $temp_stats" 1>&2
echo "performance: running crawler with ${CONCURRENT_TASKS} concurrent tasks..." 1>&2

CRAWLER_CMD="cargo run --example crawler --release -p bitcoin-peers-crawler -- \
    --address $SEED_ADDRESS \
    --port $SEED_PORT \
    --concurrent-tasks $CONCURRENT_TASKS \
    --log-level $LOG_LEVEL"
 
# Run with time and timeout, capturing stderr to temp file while also displaying it. Using
# the environment's time so that it the beef'd up GNU version, not build in shell. The
# command group `{}` is used around time+timeout+cargo so that that stderr can be redirected.
# 
# >() is process substitution, write to the inner command.
# 
# time+timeout+cargo ----stderr----> named pipe -----> tee -----> temp_stats file
#                                                       |
#                                                       +------> stderr (screen)
{ /usr/bin/env time -v timeout ${TEST_DURATION_SECONDS}s $CRAWLER_CMD; } 2> >(tee "$temp_stats" >&2) || true

end_time=$(date +%s)
# Arithmetic expansion syntax!
duration=$((end_time - start_time))

peers_discovered=$(grep -c "Listening Peer:\|Non-listening Peer:" "$temp_stats" 2>/dev/null || echo "0")
max_memory=$(grep "Maximum resident set size" "$temp_stats" | awk '{print $6}')
cpu_time=$(grep "User time" "$temp_stats" | awk '{print $4}')
sys_time=$(grep "System time" "$temp_stats" | awk '{print $4}')

# Calculate integer rate (peers per second).
if [[ $duration -gt 0 ]]; then
    rate=$((peers_discovered / duration))
else
    rate=0
fi

echo "peers=$peers_discovered"
echo "rate=$rate"
echo "memory_kb=$max_memory"
echo "cpu_user=$cpu_time"
echo "cpu_system=$sys_time"
echo "duration=$duration"
