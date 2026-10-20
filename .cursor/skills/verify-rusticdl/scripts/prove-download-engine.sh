#!/usr/bin/env bash
# Headless download-engine proof.
#
# Runs the production engine (spawn_engine + EngineCommand::Add + FileJobStore)
# against a local python3 http.server fixture. Prints the job's final state
# and compares sha256(file on disk) with the identity payload.
#
# Usage, from the repo root:
#   .cursor/skills/verify-rusticdl/scripts/prove-download-engine.sh identity
#   .cursor/skills/verify-rusticdl/scripts/prove-download-engine.sh gzip-both
#   .cursor/skills/verify-rusticdl/scripts/prove-download-engine.sh length-mismatch
#   .cursor/skills/verify-rusticdl/scripts/prove-download-engine.sh resume-206
#   .cursor/skills/verify-rusticdl/scripts/prove-download-engine.sh all
#   .cursor/skills/verify-rusticdl/scripts/prove-download-engine.sh --scenario scenario.json
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=env.sh
source "${SCRIPT_DIR}/env.sh"

SERVER="${SCRIPT_DIR}/engine_http_server.py"
DRIVER_SRC="${SCRIPT_DIR}/engine_drive.rs"
HARNESS_DIR="${ENGINE_HARNESS_DIR:-/tmp/verify-rusticdl-engine-harness}"
WORK_ROOT="${ENGINE_WORK_ROOT:-/tmp/rusticdl-engine-${RUN_ID}}"
ART_DIR="${VERIFY_ARTIFACTS_DIR}/download-engine/${RUN_ID}"
mkdir -p "${WORK_ROOT}" "${ART_DIR}"

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required" >&2
  exit 1
fi

rustc_version="$(rustc --version 2>/dev/null || true)"
if [[ ! "${rustc_version}" =~ rustc\ 1\.([0-9]+) ]]; then
  echo "rustc 1.89+ is required (docs/build.md, ci.yml). Found: ${rustc_version:-none}" >&2
  exit 1
fi
if [[ "${BASH_REMATCH[1]}" -lt 89 ]]; then
  echo "rustc 1.89+ is required. Found: ${rustc_version}" >&2
  exit 1
fi

ensure_harness() {
  mkdir -p "${HARNESS_DIR}/.cargo"
  cat >"${HARNESS_DIR}/.cargo/config.toml" <<'EOF'
[build]
rustflags = ["--cfg", "reqwest_unstable"]
EOF
  cat >"${HARNESS_DIR}/build.rs" <<'EOF'
fn main() {
    let version = std::env::var("RUSTICDL_VERSION").unwrap_or_else(|_| "0.3.6".into());
    println!("cargo:rustc-env=RUSTICDL_VERSION={version}");
    println!("cargo:rerun-if-env-changed=RUSTICDL_VERSION");
}
EOF
  cat >"${HARNESS_DIR}/Cargo.toml" <<EOF
[package]
name = "verify_rusticdl_engine"
version = "0.3.6"
edition = "2021"
publish = false

[[bin]]
name = "engine-drive"
path = "${DRIVER_SRC}"

[dependencies]
async-trait = "0.1"
dirs = "6"
futures-util = "0.3"
open = "5"
percent-encoding = "2"
reqwest = { version = "0.12", default-features = false, features = ["stream", "rustls-tls", "http2", "http3", "json"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "fs", "io-util", "io-std", "net", "sync", "time"] }
url = "2"
uuid = { version = "1", features = ["v4"] }

[target.'cfg(unix)'.dependencies]
libc = "0.2"
EOF
  export RUSTICDL_VERSION="${RUSTICDL_VERSION:-0.3.6}"
  echo "Compiling engine-drive (existing rusticdl modules, no new crates)…" >&2
  cargo build --manifest-path "${HARNESS_DIR}/Cargo.toml" --bin engine-drive >&2
}

DRIVER_BIN="${HARNESS_DIR}/target/debug/engine-drive"

sha256_file() {
  local path="$1"
  if [[ -n "${path}" && "${path}" != "-" && -f "${path}" ]]; then
    sha256sum "${path}" | awk '{print $1}'
  else
    echo "-"
  fi
}

file_len() {
  local path="$1"
  if [[ -n "${path}" && "${path}" != "-" && -f "${path}" ]]; then
    wc -c <"${path}" | tr -d ' '
  else
    echo "-"
  fi
}

match_flag() {
  local left="$1" right="$2"
  if [[ "${left}" != "-" && "${left}" == "${right}" ]]; then
    echo true
  else
    echo false
  fi
}

SERVER_PID=""
stop_server() {
  if [[ -n "${SERVER_PID}" ]] && kill -0 "${SERVER_PID}" 2>/dev/null; then
    kill "${SERVER_PID}" 2>/dev/null || true
    wait "${SERVER_PID}" 2>/dev/null || true
  fi
  SERVER_PID=""
}

run_one() {
  local label="$1"
  local mode="$2"
  local spec="$3"
  local case_dir="${WORK_ROOT}/${label}"
  local dest="${case_dir}/downloads"
  local data="${case_dir}/data"
  local log="${case_dir}/server.log"
  local manifest="${case_dir}/manifest.json"
  local out="${ART_DIR}/${label}.txt"
  mkdir -p "${dest}" "${data}"
  : >"${log}"

  local server_args=()
  if [[ "${mode}" == "case" ]]; then
    server_args=(--case "${spec}")
  else
    server_args=(--scenario "${spec}")
  fi

  python3 -u "${SERVER}" \
    "${server_args[@]}" \
    --bind 127.0.0.1 \
    --port 0 \
    --log "${log}" \
    --manifest "${manifest}" \
    >"${case_dir}/server.stdout" &
  SERVER_PID=$!

  local port=""
  local i
  for i in $(seq 1 50); do
    if ! kill -0 "${SERVER_PID}" 2>/dev/null; then
      echo "HTTP fixture exited early" >&2
      cat "${case_dir}/server.stdout" >&2 || true
      stop_server
      return 1
    fi
    port="$(awk '/^PORT / {print $2}' "${case_dir}/server.stdout" | tail -n 1)"
    if [[ -n "${port}" ]]; then
      break
    fi
    sleep 0.1
  done
  if [[ -z "${port}" ]]; then
    echo "HTTP fixture did not print a port" >&2
    stop_server
    return 1
  fi

  local url="http://127.0.0.1:${port}/payload.bin"
  local timeout=45
  if [[ "${label}" == "resume-206" ]]; then
    timeout=90
  fi

  set +e
  "${DRIVER_BIN}" \
    --url "${url}" \
    --dest "${dest}" \
    --filename payload.bin \
    --data-dir "${data}" \
    --timeout "${timeout}" \
    >"${case_dir}/driver.txt" 2>"${case_dir}/driver.err"
  local driver_rc=$?
  set -e
  stop_server

  local expected wire
  expected="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["identity_sha256"])' "${manifest}")"
  wire="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["wire_sha256"])' "${manifest}")"

  local target="-" temp="-"
  if [[ -f "${case_dir}/driver.txt" ]]; then
    target="$(awk -F= '/^target_path=/{print substr($0, index($0,"=")+1)}' "${case_dir}/driver.txt" | tail -n 1)"
    temp="$(awk -F= '/^temp_path=/{print substr($0, index($0,"=")+1)}' "${case_dir}/driver.txt" | tail -n 1)"
  fi

  local final_sha partial_sha
  final_sha="$(sha256_file "${target}")"
  partial_sha="$(sha256_file "${temp}")"

  {
    echo "CASE ${label}"
    echo "URL ${url}"
    echo "driver_exit=${driver_rc}"
    if [[ -s "${case_dir}/driver.err" ]]; then
      echo "DRIVER_ERR"
      cat "${case_dir}/driver.err"
    fi
    if [[ -f "${case_dir}/driver.txt" ]]; then
      cat "${case_dir}/driver.txt"
    fi
    echo "expected_sha256=${expected}"
    echo "final_sha256=${final_sha}"
    echo "final_len=$(file_len "${target}")"
    echo "final_matches_expected=$(match_flag "${final_sha}" "${expected}")"
    echo "wire_sha256=${wire}"
    echo "final_matches_wire=$(match_flag "${final_sha}" "${wire}")"
    echo "partial_sha256=${partial_sha}"
    echo "partial_len=$(file_len "${temp}")"
    echo "partial_matches_expected=$(match_flag "${partial_sha}" "${expected}")"
    echo "partial_matches_wire=$(match_flag "${partial_sha}" "${wire}")"
    echo "MANIFEST"
    cat "${manifest}"
    echo "SERVER_LOG"
    cat "${log}"
    echo "CASE_END ${label}"
  } | tee "${out}"

  if [[ "${driver_rc}" -ne 0 ]]; then
    return "${driver_rc}"
  fi
}

ensure_harness

mode_arg="${1:-}"
if [[ -z "${mode_arg}" ]]; then
  echo "usage: prove-download-engine.sh {identity|gzip-both|length-mismatch|resume-206|all|--scenario FILE}" >&2
  exit 2
fi

rc=0
if [[ "${mode_arg}" == "--scenario" ]]; then
  spec="${2:?scenario json required}"
  label="$(basename "${spec}" .json)"
  run_one "${label}" scenario "${spec}" || rc=$?
elif [[ "${mode_arg}" == "all" ]]; then
  for case_name in identity gzip-both length-mismatch resume-206; do
    run_one "${case_name}" case "${case_name}" || rc=$?
    echo
  done
else
  case "${mode_arg}" in
    identity|gzip-both|length-mismatch|resume-206) ;;
    *)
      echo "unknown case: ${mode_arg}" >&2
      exit 2
      ;;
  esac
  run_one "${mode_arg}" case "${mode_arg}" || rc=$?
fi

echo "Artifacts: ${ART_DIR}" >&2
exit "${rc}"
