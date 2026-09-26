#!/usr/bin/env bash
# Send one newline-delimited JSON IPC request to the desktop bridge.
# Usage:
#   ipc.sh get_status
#   ipc.sh show_window
#   ipc.sh enqueue_download <url> [suggestedFilename]
#   ipc.sh raw '<json-object>'
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=env.sh
source "${SCRIPT_DIR}/env.sh"

[[ -S "${IPC_SOCK}" ]] || { echo "IPC socket missing: ${IPC_SOCK}" >&2; exit 1; }

rid="verify-$(date +%s%N)"
cmd="${1:-get_status}"

case "${cmd}" in
  get_status|ping|show_window)
    payload=$(printf '{"protocolVersion":1,"requestId":"%s","type":"%s","payload":{}}' "${rid}" "${cmd}")
    ;;
  enqueue_download)
    url="${2:?url required}"
    name="${3:-}"
    if [[ -n "${name}" ]]; then
      payload=$(python3 - "${rid}" "${url}" "${name}" <<'PY'
import json,sys
rid,url,name=sys.argv[1:4]
print(json.dumps({
  "protocolVersion":1,
  "requestId":rid,
  "type":"enqueue_download",
  "payload":{
    "url":url,
    "suggestedFilename":name,
    "source":{"entryPoint":"verify","browser":"none","extensionVersion":"0.0.0"}
  }
}))
PY
)
    else
      payload=$(python3 - "${rid}" "${url}" <<'PY'
import json,sys
rid,url=sys.argv[1:3]
print(json.dumps({
  "protocolVersion":1,
  "requestId":rid,
  "type":"enqueue_download",
  "payload":{
    "url":url,
    "source":{"entryPoint":"verify","browser":"none","extensionVersion":"0.0.0"}
  }
}))
PY
)
    fi
    ;;
  raw)
    payload="${2:?json required}"
    ;;
  *)
    echo "Unknown ipc command: ${cmd}" >&2
    exit 1
    ;;
esac

python3 - "${IPC_SOCK}" "${payload}" <<'PY'
import socket,sys
sock_path,payload=sys.argv[1],sys.argv[2]
s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)
s.settimeout(5)
s.connect(sock_path)
s.sendall((payload+"\n").encode())
data=b""
while not data.endswith(b"\n"):
    chunk=s.recv(4096)
    if not chunk:
        break
    data+=chunk
sys.stdout.write(data.decode())
s.close()
PY
