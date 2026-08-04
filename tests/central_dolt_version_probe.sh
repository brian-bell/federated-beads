#!/usr/bin/env bash
# Probe one external Dolt server version against the supported bd client.
# EXPECT=compatible requires a complete init/create/read round trip.
# EXPECT=incompatible requires a safe failure and no readable initialized DB.

set -euo pipefail

BD_BIN="${BD_BIN:-$(command -v bd || true)}"
DOLT_BIN="${DOLT_BIN:-$(command -v dolt || true)}"
EXPECT="${EXPECT:-compatible}"
COMPAT_PARENT="${FBD_COMPAT_PARENT:-${XDG_CACHE_HOME:-$HOME/.cache}}"

[[ -x "$BD_BIN" ]] || { printf 'BD_BIN is required\n' >&2; exit 2; }
[[ -x "$DOLT_BIN" ]] || { printf 'DOLT_BIN is required\n' >&2; exit 2; }
[[ "$EXPECT" == compatible || "$EXPECT" == incompatible ]] || {
  printf 'EXPECT must be compatible or incompatible\n' >&2
  exit 2
}

mkdir -p "$COMPAT_PARENT"
ROOT="$(mktemp -d "$COMPAT_PARENT/fbd-dolt-version.XXXXXX")"
SERVER_PID=''
cleanup() {
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$ROOT"
}
trap cleanup EXIT INT TERM

export HOME="$ROOT/home"
export XDG_CONFIG_HOME="$ROOT/home/.config"
export XDG_DATA_HOME="$ROOT/home/.local/share"
export XDG_CACHE_HOME="$ROOT/home/.cache"
export BD_NON_INTERACTIVE=1
export CI=true
mkdir -p "$HOME" "$ROOT/server-data" "$ROOT/client"

port="$(python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)"

server_args=(sql-server --host 127.0.0.1 --port "$port")
if "$DOLT_BIN" sql-server --help 2>&1 | grep -q -- '--data-dir'; then
  server_args+=(--data-dir "$ROOT/server-data")
else
  server_args+=(--multi-db-dir "$ROOT/server-data")
fi

"$DOLT_BIN" "${server_args[@]}" >"$ROOT/server.log" 2>&1 &
SERVER_PID=$!
deadline=$((SECONDS + 20))
while ((SECONDS < deadline)); do
  if python3 - "$port" <<'PY' 2>/dev/null
import socket
import sys
with socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=0.25):
    pass
PY
  then
    break
  fi
  sleep 0.1
done
kill -0 "$SERVER_PID" 2>/dev/null || {
  if [[ "$EXPECT" == incompatible ]]; then
    printf 'PASS incompatible: server failed safely before a client write\n'
    exit 0
  fi
  sed -n '1,120p' "$ROOT/server.log" >&2
  exit 1
}

set +e
(
  cd "$ROOT/client"
  "$BD_BIN" init --server --external --server-host 127.0.0.1 \
    --server-port "$port" --server-user root --database versionprobe \
    --prefix versionprobe --skip-hooks --skip-agents --non-interactive
) >"$ROOT/init.log" 2>&1
init_status=$?
set -e

if [[ "$EXPECT" == incompatible ]]; then
  [[ $init_status -ne 0 ]] || {
    printf 'FAIL: expected an unsupported combination, but init succeeded\n' >&2
    exit 1
  }
  sed -n '1,80p' "$ROOT/init.log" | sed '/fixture-secret/d'
  printf 'PASS incompatible: bd rejected the server before a usable write (exit %s)\n' "$init_status"
  exit 0
fi

if [[ $init_status -ne 0 ]]; then
  sed -n '1,160p' "$ROOT/init.log" >&2
  exit "$init_status"
fi

(
  cd "$ROOT/client"
  issue_id="$("$BD_BIN" create --title 'version round trip' \
    --description 'external server compatibility probe' --type task \
    --priority 2 --json | jq -r '.id')"
  "$BD_BIN" show "$issue_id" --json >/dev/null
  "$BD_BIN" context --json | jq -e \
    '.schema_version == 1 and .dolt_mode == "server"' >/dev/null
)

printf 'PASS compatible: %s with %s\n' "$($BD_BIN version)" "$($DOLT_BIN version | head -n 1)"
