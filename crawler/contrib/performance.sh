#!/usr/bin/env bash

# Performance testing script for bitcoin-peers crawler
# Tests different concurrent task settings and measures resource usage
#
# Usage:
#   ./crawler/contrib/performance.sh
#
# Environment Variables:
#   SEED_ADDRESS              - IP address of seed node (default: 127.0.0.1)
#   SEED_PORT                 - Port of seed node (default: 8333)
#   TEST_DURATION_SECONDS     - Test duration in seconds (default: 60)
#   LOG_LEVEL                 - Log level for crawler (default: info)
#
# Examples:
#   # Basic test with defaults
#   ./crawler/contrib/performance.sh
#
#   # Test with custom seed and longer duration
#   SEED_ADDRESS=192.168.1.100 TEST_DURATION_SECONDS=120 ./crawler/contrib/performance.sh
#
#   # Save results to file
#   ./crawler/contrib/performance.sh > performance_results.txt 2>&1
#
# The script tests concurrent task settings: 1, 2, 4, 8, 16, 32
# Performance metrics go to stdout, crawler logs go to stderr.
#
# Metrics collected:
#   - Peers discovered (total count and rate per second)
#   - Peak memory usage (KB)
#   - CPU time (user and system)
#   - Test duration (actual runtime)
#
# Output streams:
#   - stdout: Performance test progress and summary table
#   - stderr: Crawler logs for each test run
#
# Examples of selective redirection:
#   # Save only performance metrics
#   ./crawler/contrib/performance.sh > performance_metrics.txt
#   
#   # Save only crawler logs  
#   ./crawler/contrib/performance.sh 2> crawler_logs.txt
#   
#   # Save both to separate files
#   ./crawler/contrib/performance.sh > metrics.txt 2> logs.txt
#
# Requirements:
#   - time command (for resource monitoring)
#   - bc command (for calculations, optional)

set -e

# Configuration
SEED_ADDRESS="${SEED_ADDRESS:-127.0.0.1}"
SEED_PORT="${SEED_PORT:-8333}"
TEST_DURATION_SECONDS="${TEST_DURATION_SECONDS:-60}"
CONCURRENT_TASKS=(1 2 4 8 16 32)
LOG_LEVEL="${LOG_LEVEL:-info}"

echo "=== Bitcoin Peers Crawler Performance Test ==="
echo "Seed: ${SEED_ADDRESS}:${SEED_PORT}"
echo "Test duration: ${TEST_DURATION_SECONDS} seconds"
echo "Testing concurrent task settings: ${CONCURRENT_TASKS[*]}"
echo

# Array to store results for summary table
declare -A results

# Runs the crawler with specified concurrent tasks and collects performance metrics.
# 
# Args:
#   $1 - Number of concurrent tasks to test
#
# Metrics collected:
#   - Peer discovery count (from crawler logs)
#   - Peak memory usage (from /usr/bin/time)
#   - CPU time (from /usr/bin/time)
#   - Discovery rate (calculated)
#
# Output:
#   - Performance summary to stdout
#   - Crawler logs to stderr
#   - Results stored in global 'results' associative array
run_test() {
    local concurrent=$1
    
    echo "Testing with $concurrent concurrent tasks..."
    
    # Start time tracking
    local start_time=$(date +%s)
    
    # Create temp file for combined stderr (crawler logs + time stats)
    local temp_combined=$(mktemp)
    
    # Run the crawler - all stderr goes to temp file
    if [[ -n "$TIME_CMD" ]]; then
        timeout ${TEST_DURATION_SECONDS}s $TIME_CMD -v cargo run --example crawler --release -p bitcoin-peers-crawler -- \
            --address "$SEED_ADDRESS" \
            --port "$SEED_PORT" \
            --concurrent-tasks "$concurrent" \
            --log-level "$LOG_LEVEL" \
            2>"$temp_combined" || true
    else
        # Fallback without time stats
        timeout ${TEST_DURATION_SECONDS}s cargo run --example crawler --release -p bitcoin-peers-crawler -- \
            --address "$SEED_ADDRESS" \
            --port "$SEED_PORT" \
            --concurrent-tasks "$concurrent" \
            --log-level "$LOG_LEVEL" \
            2>"$temp_combined" || true
    fi
    
    local end_time=$(date +%s)
    local duration=$((end_time - start_time))
    
    # Extract key metrics from the combined stderr
    local peers_discovered=$(grep -c "Listening Peer:\|Non-listening Peer:" "$temp_combined" 2>/dev/null || echo "0")
    
    if [[ -n "$TIME_CMD" ]]; then
        local max_memory=$(grep "Maximum resident set size" "$temp_combined" | awk '{print $6}' || echo "0")
        local cpu_time=$(grep "User time" "$temp_combined" | awk '{print $4}' || echo "0")
        local sys_time=$(grep "System time" "$temp_combined" | awk '{print $4}' || echo "0")
    else
        local max_memory="N/A"
        local cpu_time="N/A"
        local sys_time="N/A"
    fi
    
    local rate=$(echo "scale=2; $peers_discovered / $duration" | bc -l 2>/dev/null || echo "N/A")
    
    # Store results for summary
    results["${concurrent}_duration"]="$duration"
    results["${concurrent}_peers"]="$peers_discovered"
    results["${concurrent}_memory"]="$max_memory"
    results["${concurrent}_cpu"]="$cpu_time"
    results["${concurrent}_rate"]="$rate"
    
    echo "  Completed: $peers_discovered peers discovered in ${duration}s (Max memory: ${max_memory}KB, Rate: ${rate} peers/sec)"
    
    # Output crawler logs to stderr so user can see them if desired
    echo "--- Crawler output for $concurrent concurrent tasks ---" >&2
    grep -v "Maximum resident set size\|User time\|System time\|Elapsed (wall clock)" "$temp_combined" >&2 || true
    echo "--- End crawler output ---" >&2
    echo >&2
    
    # Clean up temp file immediately
    rm -f "$temp_combined"
}

# Check dependencies
if ! command -v bc &> /dev/null; then
    echo "Warning: 'bc' not found, peer rate calculations will be disabled"
fi

# Check for GNU time command (needed for -v flag and memory stats)
TIME_CMD=""
if command -v gtime &> /dev/null; then
    TIME_CMD="gtime"  # GNU time on macOS/BSD systems
elif command -v time &> /dev/null && time -v true &> /dev/null 2>&1; then
    TIME_CMD="time"   # GNU time on Linux systems
else
    echo "Warning: GNU time not found, memory and CPU stats will be disabled"
    echo "  On NixOS: nix shell nixpkgs#time"
    echo "  On macOS: brew install gnu-time"
fi

# Build in release mode first
echo "Building crawler in release mode..."
cargo build --example crawler --release -p bitcoin-peers-crawler
echo

# Run tests for each concurrent task setting
for concurrent in "${CONCURRENT_TASKS[@]}"; do
    run_test "$concurrent"
done

# Generate summary report
echo "=== Performance Test Summary ==="
echo "| Tasks | Duration | Peers | Max Mem (KB) | CPU Time | Peers/sec |"
echo "|-------|----------|-------|--------------|----------|-----------|"

for concurrent in "${CONCURRENT_TASKS[@]}"; do
    if [[ -n "${results[${concurrent}_duration]}" ]]; then
        printf "| %5d | %8s | %5s | %12s | %8s | %9s |\n" \
            "$concurrent" \
            "${results[${concurrent}_duration]}s" \
            "${results[${concurrent}_peers]}" \
            "${results[${concurrent}_memory]}" \
            "${results[${concurrent}_cpu]}s" \
            "${results[${concurrent}_rate]}"
    fi
done

echo
echo "Performance test complete!"
