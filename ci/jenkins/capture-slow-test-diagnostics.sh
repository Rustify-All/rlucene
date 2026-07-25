#!/usr/bin/env bash

set -u

launcher_pid="${1:?nextest launcher PID is required}"
delay_seconds="${2:-300}"
output_file="${3:-nextest-diagnostics.log}"
poll_seconds=5

descendants_of() {
  local parent_pid="$1"
  local child_pid

  while read -r child_pid; do
    if [ -n "$child_pid" ]; then
      printf '%s\n' "$child_pid"
      descendants_of "$child_pid"
    fi
  done < <(
    ps -eo pid=,ppid= |
      awk -v parent_pid="$parent_pid" '$2 == parent_pid { print $1 }'
  )
}

find_nextest_runner() {
  local pid
  local command_name
  local command_line

  for pid in "$launcher_pid" $(descendants_of "$launcher_pid"); do
    command_name="$(ps -o comm= -p "$pid" 2>/dev/null || true)"
    command_line="$(ps -o args= -p "$pid" 2>/dev/null || true)"
    if [[ "$command_name" == *cargo-nextest* ]] ||
      [[ "$command_line" == *"cargo-nextest nextest run"* ]]; then
      printf '%s\n' "$pid"
      return 0
    fi
  done

  return 1
}

capture_file() {
  local path="$1"

  printf '\n===== %s =====\n' "$path"
  if [ -r "$path" ]; then
    cat "$path"
  else
    printf 'unavailable or unreadable\n'
  fi
}

capture_thread_state() {
  local process_pid="$1"
  local task_path
  local thread_id

  for task_path in "/proc/$process_pid"/task/*; do
    if [ ! -d "$task_path" ]; then
      continue
    fi
    thread_id="${task_path##*/}"
    printf '\n----- PID %s TID %s -----\n' "$process_pid" "$thread_id"
    capture_file "$task_path/comm"
    capture_file "$task_path/status"
    capture_file "$task_path/wchan"
    capture_file "$task_path/syscall"
    capture_file "$task_path/stack"
  done
}

capture_userspace_backtrace() {
  local process_pid="$1"

  printf '\n===== userspace backtrace for PID %s =====\n' "$process_pid"
  if command -v eu-stack >/dev/null 2>&1; then
    timeout --kill-after=5s 20s eu-stack -p "$process_pid" || true
  elif command -v gdb >/dev/null 2>&1; then
    timeout --kill-after=5s 20s \
      gdb --batch --quiet -p "$process_pid" \
      -ex 'set pagination off' \
      -ex 'set print thread-events off' \
      -ex 'info threads' \
      -ex 'thread apply all bt' \
      -ex 'detach' \
      -ex 'quit' || true
  elif command -v pstack >/dev/null 2>&1; then
    timeout --kill-after=5s 20s pstack "$process_pid" || true
  else
    printf 'eu-stack, gdb, and pstack are unavailable; no userspace backtrace captured.\n'
  fi
}

declare -A captured_pids=()
nextest_runner_pid=''
header_written='false'

while kill -0 "$launcher_pid" 2>/dev/null; do
  if [ -z "$nextest_runner_pid" ] ||
    ! kill -0 "$nextest_runner_pid" 2>/dev/null; then
    nextest_runner_pid="$(find_nextest_runner || true)"
  fi

  if [ -n "$nextest_runner_pid" ]; then
    mapfile -t slow_test_pids < <(
      ps -eo pid=,ppid=,etimes= |
        awk -v parent_pid="$nextest_runner_pid" \
          -v minimum_seconds="$delay_seconds" \
          '$2 == parent_pid && $3 >= minimum_seconds { print $1 }'
    )

    new_slow_test_pids=()
    for process_pid in "${slow_test_pids[@]}"; do
      if [ -z "${captured_pids[$process_pid]:-}" ]; then
        captured_pids["$process_pid"]='true'
        new_slow_test_pids+=("$process_pid")
      fi
    done

    if [ "${#new_slow_test_pids[@]}" -gt 0 ]; then
      if [ "$header_written" != 'true' ]; then
        {
          printf 'Slow-test diagnostics at %s seconds per test\n' \
            "$delay_seconds"
          printf 'First capture at: '
          date -u '+%Y-%m-%dT%H:%M:%SZ'
          printf 'Launcher PID: %s\n' "$launcher_pid"
          printf 'Nextest runner PID: %s\n' "$nextest_runner_pid"
          printf '\n===== system =====\n'
          uname -a
          uptime
          if command -v free >/dev/null 2>&1; then
            free -h
          fi
          if command -v vmstat >/dev/null 2>&1; then
            vmstat 1 5
          fi
          df -h . /tmp "${CARGO_TARGET_DIR:-.}"
        } >> "$output_file" 2>&1
        header_written='true'
      fi

      {
        printf '\n===== process tree =====\n'
        ps -eo \
          user=,pid=,ppid=,pgid=,sid=,stat=,etimes=,pcpu=,pmem=,comm=,wchan:32=,args= \
          --forest
        if kill -USR1 "$nextest_runner_pid" 2>/dev/null; then
          printf '\nSent SIGUSR1 to nextest; running-test details were written to nextest.log.\n'
        else
          printf '\nUnable to send SIGUSR1 to nextest.\n'
        fi
      } >> "$output_file" 2>&1

      sleep 2

      for process_pid in "${new_slow_test_pids[@]}"; do
        {
          printf '\n########################################\n'
          printf 'Slow test process PID: %s\n' "$process_pid"
          printf 'Captured at: '
          date -u '+%Y-%m-%dT%H:%M:%SZ'
          ps -L -p "$process_pid" \
            -o pid=,tid=,ppid=,stat=,etimes=,psr=,pcpu=,pmem=,comm=,wchan:32=,args=
          capture_file "/proc/$process_pid/status"
          capture_file "/proc/$process_pid/io"
          capture_file "/proc/$process_pid/limits"
          capture_thread_state "$process_pid"
          capture_userspace_backtrace "$process_pid"
        } >> "$output_file" 2>&1
      done
    fi
  fi

  sleep "$poll_seconds"
done
