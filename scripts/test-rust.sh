#!/usr/bin/env sh
set -eu

# Keep the full Rust suite deterministic on macOS, where shells commonly
# inherit a 256-descriptor soft limit. SQLite WAL files, HTTP fixtures, Tokio
# runtimes, and child-process pipes can otherwise exhaust that budget when
# libtest runs several FD-heavy tests concurrently.

repo_root=$(CDPATH= cd "$(dirname "$0")/.." && pwd)
cd "$repo_root"

target_fd_limit=${TERMAL_TEST_FD_LIMIT:-4096}
test_threads=${TERMAL_TEST_THREADS:-${RUST_TEST_THREADS:-4}}

require_positive_integer() {
    variable_name=$1
    variable_value=$2
    case "$variable_value" in
        ''|*[!0-9]*|0)
            echo "$variable_name must be a positive integer; got '$variable_value'." >&2
            exit 2
            ;;
    esac
}

require_positive_integer TERMAL_TEST_FD_LIMIT "$target_fd_limit"
require_positive_integer TERMAL_TEST_THREADS "$test_threads"

current_soft_limit=$(ulimit -S -n 2>/dev/null || echo unknown)
hard_limit=$(ulimit -H -n 2>/dev/null || echo unknown)
desired_soft_limit=$target_fd_limit

case "$hard_limit" in
    ''|*[!0-9]*) ;;
    *)
        if [ "$hard_limit" -lt "$desired_soft_limit" ]; then
            desired_soft_limit=$hard_limit
        fi
        ;;
esac

case "$current_soft_limit" in
    ''|*[!0-9]*) ;;
    *)
        if [ "$current_soft_limit" -lt "$desired_soft_limit" ]; then
            if ! ulimit -S -n "$desired_soft_limit" 2>/dev/null; then
                echo "Warning: could not raise the file-descriptor soft limit from $current_soft_limit to $desired_soft_limit." >&2
            fi
        fi
        ;;
esac

effective_soft_limit=$(ulimit -S -n 2>/dev/null || echo unknown)
echo "Rust test gate: fd soft limit=$effective_soft_limit, test threads=$test_threads"

RUST_TEST_THREADS=$test_threads
export RUST_TEST_THREADS
exec cargo test "$@"
