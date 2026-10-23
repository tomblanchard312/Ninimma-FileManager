#!/usr/bin/env bash
# Ninimma modification: independent project packaging and branding.
# Benchmarks for the opt-in benchmark suite: startup and idle (Linux and FreeBSD
# with a display; needs /proc on Linux for idle), then the in-process
# benchmarks (large folders, thumbnails, search, transfers).
#
#   tools/bench.sh            everything
#   tools/bench.sh startup    startup and idle only
#   tools/bench.sh inprocess  the cargo benchmarks only
#
# Everything runs on files made here, in a temporary folder, with HOME and
# the XDG folders pointed there too, so your settings, thumbnails cache
# and search index are never read or written. NINIMMA_BENCH_DIR picks
# the disk for the fixtures; NINIMMA_BENCH_LARGE=1 adds 100,000 files.
set -euo pipefail
cd "$(dirname "$0")/.."

what="${1:-all}"
# cargo and rustup must still be found once HOME points at a temp folder.
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
runs="${STARTUP_RUNS:-5}"
idle_secs="${IDLE_SECS:-30}"

machine() {
    echo "== machine"
    echo "commit:  $(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
    echo "os:      $(uname -sr)"
    if [ -r /proc/cpuinfo ]; then
        echo "cpu:     $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ //') ($(nproc) threads)"
        echo "memory:  $(awk '/MemTotal/ {printf "%.0f GiB", $2/1048576}' /proc/meminfo)"
    fi
    echo "fixtures: $(df -T "${NINIMMA_BENCH_DIR:-${TMPDIR:-/tmp}}" | awk 'NR==2 {print $2" ("$1")"}')"
    echo "rustc:   $(rustc --version)"
}

startup() {
    cargo build --release --quiet
    local bin=target/release/ninimma
    local tmp
    tmp="$(mktemp -d "${NINIMMA_BENCH_DIR:-${TMPDIR:-/tmp}}/fe-bench.XXXXXX")"
    trap 'rm -rf "$tmp"' RETURN
    mkdir -p "$tmp/home" "$tmp/folder"
    for i in $(seq 1 1000); do : > "$tmp/folder/file $i.txt"; done

    echo "== startup (launch to first frame with a 1,000-file folder listed)"
    local pid=""
    for i in $(seq 1 "$runs"); do
        local log="$tmp/run$i.log"
        HOME="$tmp/home" XDG_CONFIG_HOME="$tmp/home/.config" XDG_DATA_HOME="$tmp/home/.local/share" \
            XDG_CACHE_HOME="$tmp/home/.cache" NINIMMA_STARTUP_PROBE=1 \
            "$bin" "$tmp/folder" 2>"$log" &
        pid=$!
        for _ in $(seq 1 300); do
            grep -q '^STARTUP' "$log" && break
            sleep 0.05
        done
        echo "run $i: $(grep -m1 '^STARTUP' "$log" || echo 'no first frame within 15 s')"
        if [ "$i" -lt "$runs" ]; then kill "$pid"; wait "$pid" 2>/dev/null || true; fi
    done

    if [ -r "/proc/$pid/stat" ]; then
        echo "== idle (window open, untouched, ${idle_secs} s after 5 s to settle)"
        sleep 5
        local hz ticks0 ticks1
        hz="$(getconf CLK_TCK)"
        ticks0="$(awk '{print $14+$15}' "/proc/$pid/stat")"
        sleep "$idle_secs"
        ticks1="$(awk '{print $14+$15}' "/proc/$pid/stat")"
        awk -v d=$((ticks1 - ticks0)) -v hz="$hz" -v s="$idle_secs" \
            'BEGIN {printf "cpu:     %.2f %% of one core\n", 100 * d / hz / s}'
        echo "rss:     $(awk '/VmRSS/ {printf "%.0f MiB", $2/1024}' "/proc/$pid/status")"
        echo "threads: $(awk '/Threads/ {print $2}' "/proc/$pid/status")"
    fi
    kill "$pid"; wait "$pid" 2>/dev/null || true
}

inprocess() {
    echo "== in-process benchmarks"
    local tmp
    tmp="$(mktemp -d)"
    # Thumbnail and index code look up cache folders; keep them here.
    HOME="$tmp" XDG_CACHE_HOME="$tmp/.cache" XDG_DATA_HOME="$tmp/.local/share" \
        cargo test --release --quiet bench_ -- --ignored --nocapture --test-threads=1 2>&1 \
        | grep -oE 'BENCH.*|panicked.*|FAILED.*'
    rm -rf "$tmp"
}

machine
case "$what" in
    startup) startup ;;
    inprocess) inprocess ;;
    all) startup; inprocess ;;
    *) echo "usage: $0 [all|startup|inprocess]" >&2; exit 2 ;;
esac
