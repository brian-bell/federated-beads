#!/usr/bin/env bash
# Black-box compatibility probe for federated-beads-kfv.1.
#
# This is intentionally separate from `cargo test`: it needs real, pinned bd
# and Dolt executables and launches isolated repositories and local servers.

set -euo pipefail

BD_BIN="${BD_BIN:-$(command -v bd || true)}"
DOLT_BIN="${DOLT_BIN:-$(command -v dolt || true)}"
KEEP_COMPAT_FIXTURE="${KEEP_COMPAT_FIXTURE:-0}"
COMPAT_PARENT="${FBD_COMPAT_PARENT:-${XDG_CACHE_HOME:-$HOME/.cache}}"

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

[[ -x "$BD_BIN" ]] || fail 'set BD_BIN to an executable bd binary'
[[ -x "$DOLT_BIN" ]] || fail 'set DOLT_BIN to an executable dolt binary'
command -v jq >/dev/null || fail 'jq is required'
command -v openssl >/dev/null || fail 'openssl is required'
command -v python3 >/dev/null || fail 'python3 is required'

mkdir -p "$COMPAT_PARENT"
ROOT="$(mktemp -d "$COMPAT_PARENT/fbd-central-dolt.XXXXXX")"
FIXTURE_HOME="$ROOT/home"
RESULTS="$ROOT/results"
SERVER_PID=''
mkdir -p "$FIXTURE_HOME" "$RESULTS"

cleanup() {
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [[ "$KEEP_COMPAT_FIXTURE" == 1 ]]; then
    printf 'Fixture retained at %s\n' "$ROOT"
  else
    rm -rf "$ROOT"
  fi
}
trap cleanup EXIT INT TERM

export HOME="$FIXTURE_HOME"
export XDG_CONFIG_HOME="$FIXTURE_HOME/.config"
export XDG_DATA_HOME="$FIXTURE_HOME/.local/share"
export XDG_CACHE_HOME="$FIXTURE_HOME/.cache"
export BD_NON_INTERACTIVE=1
export CI=true

"$DOLT_BIN" config --global --add user.name 'fbd compatibility probe' >/dev/null
"$DOLT_BIN" config --global --add user.email 'compat@example.invalid' >/dev/null

log() {
  printf '\n== %s ==\n' "$*"
}

record() {
  printf '%s\n' "$2" >"$RESULTS/$1"
}

bd_at() {
  local dir="$1"
  shift
  (
    cd "$dir"
    "$BD_BIN" "$@"
  )
}

dolt_at() {
  local dir="$1"
  shift
  (
    cd "$dir"
    "$DOLT_BIN" "$@"
  )
}

init_repo() {
  local dir="$1"
  local prefix="$2"
  mkdir -p "$dir"
  (
    cd "$dir"
    "$BD_BIN" init --prefix "$prefix" --skip-hooks --skip-agents \
      --non-interactive >/dev/null
  )
}

clone_repo() {
  local dir="$1"
  local remote="$2"
  mkdir -p "$dir"
  (
    cd "$dir"
    "$BD_BIN" init --remote "$remote" --skip-hooks --skip-agents \
      --non-interactive >/dev/null
  )
}

create_issue() {
  local dir="$1"
  local title="$2"
  bd_at "$dir" create --title "$title" --description "compatibility fixture: $title" \
    --type task --priority 2 --json | jq -r '.id'
}

context_field() {
  local dir="$1"
  local field="$2"
  bd_at "$dir" context --json | jq -r ".$field"
}

head_hash() {
  local db_dir="$1"
  dolt_at "$db_dir" log -n 1 --oneline | awk 'NR == 1 {print $1}'
}

remote_head() {
  local remote="$1"
  local inspect_dir
  inspect_dir="$(mktemp -d "$ROOT/remote-head.XXXXXX")"
  rm -rf "$inspect_dir"
  "$DOLT_BIN" clone "file://$remote" "$inspect_dir" >/dev/null
  head_hash "$inspect_dir"
  rm -rf "$inspect_dir"
}

remote_has_branch() {
  local remote="$1"
  local branch="$2"
  local inspect_dir
  inspect_dir="$(mktemp -d "$ROOT/remote-branch.XXXXXX")"
  rm -rf "$inspect_dir"
  "$DOLT_BIN" clone "file://$remote" "$inspect_dir" >/dev/null
  if dolt_at "$inspect_dir" branch -a | sed 's/^[*[:space:]]*//' | grep -Eq "(^|/)${branch}$"; then
    rm -rf "$inspect_dir"
    return 0
  fi
  rm -rf "$inspect_dir"
  return 1
}

expect_failure() {
  local output_file="$1"
  shift
  set +e
  "$@" >"$output_file" 2>&1
  local status=$?
  set -e
  [[ $status -ne 0 ]] || fail "expected failure: $*"
  printf '%s' "$status"
}

wait_for_port() {
  local host="$1"
  local port="$2"
  local deadline=$((SECONDS + 20))
  while ((SECONDS < deadline)); do
    if python3 - "$host" "$port" <<'PY' 2>/dev/null
import socket
import sys

with socket.create_connection((sys.argv[1], int(sys.argv[2])), timeout=0.25):
    pass
PY
    then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

free_port() {
  python3 - <<'PY'
import socket

with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
}

log 'versions'
BD_VERSION="$($BD_BIN version)"
DOLT_VERSION="$($DOLT_BIN version)"
printf '%s\n%s\n' "$BD_VERSION" "$DOLT_VERSION"
record versions.txt "$BD_VERSION
$DOLT_VERSION"

log 'A: independent histories fail safely against one branch'
A="$ROOT/a-independent"
init_repo "$A/alpha" alpha
init_repo "$A/beta" beta
alpha_id="$(create_issue "$A/alpha" 'alpha-owned')"
beta_id="$(create_issue "$A/beta" 'beta-owned')"
bd_at "$A/alpha" dolt remote add origin "file://$A/authority"
bd_at "$A/beta" dolt remote add origin "file://$A/authority"
bd_at "$A/alpha" dolt push >/dev/null
a_before="$(remote_head "$A/authority")"
a_push_status="$(expect_failure "$RESULTS/a-beta-push.txt" bd_at "$A/beta" dolt push)"
a_after="$(remote_head "$A/authority")"
[[ "$a_before" == "$a_after" ]] || fail 'rejected independent push changed authority history'
a_pull_status="$(expect_failure "$RESULTS/a-beta-pull.txt" bd_at "$A/beta" dolt pull)"
bd_at "$A/beta" show "$beta_id" --json >/dev/null
set +e
bd_at "$A/beta" show "$alpha_id" --json >/dev/null 2>&1
cross_read=$?
set -e
[[ $cross_read -ne 0 ]] || fail 'failed independent merge leaked alpha into beta'
record a-summary.txt "authority_before=$a_before
authority_after=$a_after
beta_push_exit=$a_push_status
beta_pull_exit=$a_pull_status"

log 'B: shared seed supports pull/merge/retry but shares identity and prefix'
B="$ROOT/b-shared-seed"
init_repo "$B/seed" shared
seed_id="$(create_issue "$B/seed" 'seed issue')"
bd_at "$B/seed" dolt remote add origin "file://$B/authority"
bd_at "$B/seed" dolt push >/dev/null
clone_repo "$B/client-a" "file://$B/authority"
clone_repo "$B/client-b" "file://$B/authority"
seed_project="$(context_field "$B/seed" project_id)"
[[ "$(context_field "$B/client-a" project_id)" == "$seed_project" ]]
[[ "$(context_field "$B/client-b" project_id)" == "$seed_project" ]]
[[ "$(create_issue "$B/client-a" 'prefix probe')" == shared-* ]]

b_a_id="$(create_issue "$B/client-a" 'client-a serial')"
bd_at "$B/client-a" dolt push >/dev/null
bd_at "$B/client-b" dolt pull >/dev/null
b_b_id="$(create_issue "$B/client-b" 'client-b serial')"
bd_at "$B/client-b" dep add "$b_b_id" "$b_a_id" >/dev/null
bd_at "$B/client-b" dolt push >/dev/null
bd_at "$B/client-a" dolt pull >/dev/null
bd_at "$B/client-a" blocked --json | jq -e --arg id "$b_b_id" 'any(.[]; .id == $id)' >/dev/null

div_a="$(create_issue "$B/client-a" 'divergent-a')"
div_b="$(create_issue "$B/client-b" 'divergent-b')"
bd_at "$B/client-a" dolt push >/dev/null
b_nff_status="$(expect_failure "$RESULTS/b-divergent-push.txt" bd_at "$B/client-b" dolt push)"
bd_at "$B/client-b" dolt pull >/dev/null
bd_at "$B/client-b" dolt push >/dev/null
bd_at "$B/client-a" dolt pull >/dev/null
bd_at "$B/client-a" show "$div_a" --json >/dev/null
bd_at "$B/client-a" show "$div_b" --json >/dev/null

b_idempotent_before="$(remote_head "$B/authority")"
bd_at "$B/client-a" dolt pull >/dev/null
bd_at "$B/client-a" dolt push >/dev/null
b_idempotent_after="$(remote_head "$B/authority")"
[[ "$b_idempotent_before" == "$b_idempotent_after" ]] || fail 'no-change sync changed history'

clone_repo "$B/conflict-a" "file://$B/authority"
clone_repo "$B/conflict-b" "file://$B/authority"
bd_at "$B/conflict-a" update "$seed_id" --title 'conflict from a' >/dev/null
bd_at "$B/conflict-b" update "$seed_id" --title 'conflict from b' >/dev/null
bd_at "$B/conflict-a" dolt push >/dev/null
b_conflict_push_status="$(expect_failure "$RESULTS/b-conflict-push.txt" bd_at "$B/conflict-b" dolt push)"
b_conflict_pull_status="$(expect_failure "$RESULTS/b-conflict-pull.txt" bd_at "$B/conflict-b" dolt pull)"
[[ "$(bd_at "$B/conflict-b" show "$seed_id" --json | jq -r '.[0].title')" == 'conflict from b' ]]

clone_repo "$B/fresh" "file://$B/authority"
bd_at "$B/fresh" show "$div_a" --json >/dev/null
bd_at "$B/fresh" show "$div_b" --json >/dev/null
record b-summary.txt "project_id=$seed_project
prefix=shared
non_fast_forward_exit=$b_nff_status
conflict_push_exit=$b_conflict_push_status
conflict_pull_exit=$b_conflict_pull_status
idempotent_before=$b_idempotent_before
idempotent_after=$b_idempotent_after"

log 'C: isolated branches require direct Dolt checkout and promotion'
C="$ROOT/c-isolated-branch"
clone_repo "$C/client" "file://$B/authority"
client_database="$(context_field "$C/client" database)"
client_db="$C/client/.beads/embeddeddolt/$client_database"
bd_at "$C/client" branch client-work >/dev/null
dolt_at "$client_db" checkout client-work >/dev/null
c_issue="$(create_issue "$C/client" 'branch-only issue')"
c_bd_push_output="$RESULTS/c-bd-push.txt"
bd_at "$C/client" dolt push >"$c_bd_push_output" 2>&1
set +e
remote_has_branch "$B/authority" client-work
c_bd_published=$?
set -e
[[ $c_bd_published -ne 0 ]] || fail 'bd unexpectedly published an untracked work branch'
dolt_at "$client_db" push origin client-work >/dev/null
remote_has_branch "$B/authority" client-work || fail 'direct Dolt push did not publish work branch'
"$DOLT_BIN" clone "file://$B/authority" "$C/promoter" >/dev/null
dolt_at "$C/promoter" merge remotes/origin/client-work --no-edit >/dev/null
dolt_at "$C/promoter" push origin main >/dev/null
clone_repo "$C/fresh" "file://$B/authority"
bd_at "$C/fresh" show "$c_issue" --json >/dev/null
record c-summary.txt 'stock_bd_branch_publish=false
direct_dolt_promotion=true'

log 'D: separate authorities preserve independent identity and attribution'
D="$ROOT/d-separate-authorities"
init_repo "$D/alpha" alpha
init_repo "$D/beta" beta
d_alpha_id="$(create_issue "$D/alpha" 'isolated alpha')"
d_beta_id="$(create_issue "$D/beta" 'isolated beta')"
d_alpha_project="$(context_field "$D/alpha" project_id)"
d_beta_project="$(context_field "$D/beta" project_id)"
[[ "$d_alpha_project" != "$d_beta_project" ]]
bd_at "$D/alpha" dolt remote add origin "file://$D/alpha-authority"
bd_at "$D/beta" dolt remote add origin "file://$D/beta-authority"
bd_at "$D/alpha" dolt push >/dev/null
bd_at "$D/beta" dolt push >/dev/null
clone_repo "$D/alpha-fresh" "file://$D/alpha-authority"
clone_repo "$D/beta-fresh" "file://$D/beta-authority"
[[ "$(context_field "$D/alpha-fresh" project_id)" == "$d_alpha_project" ]]
[[ "$(context_field "$D/beta-fresh" project_id)" == "$d_beta_project" ]]
bd_at "$D/alpha-fresh" show "$d_alpha_id" --json >/dev/null
bd_at "$D/beta-fresh" show "$d_beta_id" --json >/dev/null
set +e
bd_at "$D/alpha-fresh" show "$d_beta_id" --json >/dev/null 2>&1
d_alpha_cross=$?
bd_at "$D/beta-fresh" show "$d_alpha_id" --json >/dev/null 2>&1
d_beta_cross=$?
set -e
[[ $d_alpha_cross -ne 0 && $d_beta_cross -ne 0 ]] || fail 'separate authority leaked cross-project data'
record d-summary.txt "alpha_project_id=$d_alpha_project
beta_project_id=$d_beta_project
alpha_prefix=alpha
beta_prefix=beta
cross_project_reads=false"

log 'backup and restore preserve state, identity, branches, and history'
BACKUP="$ROOT/backup-restore"
bd_at "$D/alpha" branch retained-branch >/dev/null
source_history="$(head_hash "$D/alpha/.beads/embeddeddolt/alpha")"
bd_at "$D/alpha" backup init "$BACKUP/archive" >/dev/null
bd_at "$D/alpha" backup sync >/dev/null
init_repo "$BACKUP/restored" placeholder
bd_at "$BACKUP/restored" backup restore --force "$BACKUP/archive" >/dev/null
[[ "$(context_field "$BACKUP/restored" project_id)" == "$d_alpha_project" ]]
bd_at "$BACKUP/restored" show "$d_alpha_id" --json >/dev/null
bd_at "$BACKUP/restored" branch | grep -q retained-branch
restored_database="$(context_field "$BACKUP/restored" database)"
restored_history="$(head_hash "$BACKUP/restored/.beads/embeddeddolt/$restored_database")"
[[ "$restored_history" == "$source_history" ]] || fail 'backup restore changed main history head'
record backup-summary.txt "source_head=$source_history
restored_head=$restored_history
project_id_preserved=true
branch_preserved=true"

log 'authentication, credential revocation, and TLS trust boundaries'
TLS="$ROOT/tls-server"
mkdir -p "$TLS/authority-data" "$TLS/certs" "$TLS/admin-client" \
  "$TLS/direct-client" "$TLS/remote-source"
SQL_PORT="$(free_port)"
REMOTE_PORT="$(free_port)"
while [[ "$REMOTE_PORT" == "$SQL_PORT" ]]; do REMOTE_PORT="$(free_port)"; done

init_repo "$TLS/remote-source" remoteprobe
remote_seed_id="$(create_issue "$TLS/remote-source" 'remote seed')"
remote_source_database="$(context_field "$TLS/remote-source" database)"
remote_source_db="$TLS/remote-source/.beads/embeddeddolt/$remote_source_database"
dolt_at "$remote_source_db" backup sync-url "file://$TLS/remote-seed-backup" >/dev/null
(
  cd "$TLS/authority-data"
  "$DOLT_BIN" backup restore "file://$TLS/remote-seed-backup" remoteprobe >/dev/null
)

openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
  -subj '/CN=fbd-compat-ca' -keyout "$TLS/certs/ca.key" \
  -out "$TLS/certs/ca.crt" >/dev/null 2>&1
openssl req -newkey rsa:2048 -nodes -subj '/CN=localhost' \
  -keyout "$TLS/certs/server.key" -out "$TLS/certs/server.csr" >/dev/null 2>&1
printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' >"$TLS/certs/server.ext"
openssl x509 -req -days 1 -in "$TLS/certs/server.csr" \
  -CA "$TLS/certs/ca.crt" -CAkey "$TLS/certs/ca.key" -CAcreateserial \
  -extfile "$TLS/certs/server.ext" -out "$TLS/certs/server.crt" >/dev/null 2>&1

cat >"$TLS/server-plain.yaml" <<EOF
log_level: info
behavior:
  autocommit: true
listener:
  host: 127.0.0.1
  port: $SQL_PORT
data_dir: "$TLS/authority-data"
cfg_dir: "$TLS/authority-data/.doltcfg"
remotesapi:
  port: $REMOTE_PORT
EOF

"$DOLT_BIN" sql-server --config "$TLS/server-plain.yaml" >"$RESULTS/plain-server.log" 2>&1 &
SERVER_PID=$!
wait_for_port 127.0.0.1 "$SQL_PORT" || fail 'plain Dolt SQL server did not become ready'
wait_for_port 127.0.0.1 "$REMOTE_PORT" || fail 'plain Dolt remotesapi did not become ready'

(
  cd "$TLS/admin-client"
  "$BD_BIN" init --server --external --server-host 127.0.0.1 \
    --server-port "$SQL_PORT" --server-user root --database directprobe \
    --prefix directprobe --skip-hooks --skip-agents --non-interactive >/dev/null
)
bd_at "$TLS/admin-client" sql \
  "CREATE USER 'compat'@'%' IDENTIFIED BY 'fixture-secret'; GRANT ALL PRIVILEGES ON *.* TO 'compat'@'%' WITH GRANT OPTION; GRANT CLONE_ADMIN ON *.* TO 'compat'@'%';" >/dev/null

(
  cd "$TLS/direct-client"
  BEADS_DOLT_PASSWORD=fixture-secret "$BD_BIN" init --server --external --server-host 127.0.0.1 \
    --server-port "$SQL_PORT" --server-user compat --database tlsprobe \
    --prefix tlsprobe --skip-hooks --skip-agents \
    --non-interactive >/dev/null
)
tls_issue="$(BEADS_DOLT_PASSWORD=fixture-secret \
  create_issue "$TLS/direct-client" 'authenticated direct write')"

auth_wrong_status="$(expect_failure "$RESULTS/auth-wrong-password.txt" env \
  BEADS_DOLT_PASSWORD=wrong "$BD_BIN" -C "$TLS/direct-client" show "$tls_issue" --json)"

bd_at "$TLS/remote-source" dolt remote add authority \
  "http://127.0.0.1:$REMOTE_PORT/remoteprobe"
remote_update_id="$(create_issue "$TLS/remote-source" 'authenticated remote update')"
remote_wrong_status="$(expect_failure "$RESULTS/remote-wrong-password.txt" env \
  DOLT_REMOTE_USER=compat DOLT_REMOTE_PASSWORD=wrong "$BD_BIN" \
  -C "$TLS/remote-source" dolt push --remote authority)"
DOLT_REMOTE_USER=compat DOLT_REMOTE_PASSWORD=fixture-secret \
  bd_at "$TLS/remote-source" dolt push --remote authority >/dev/null

bd_at "$TLS/admin-client" sql \
  "ALTER USER 'compat'@'%' IDENTIFIED BY 'rotated-secret';" >/dev/null
auth_revoked_status="$(expect_failure "$RESULTS/auth-revoked-password.txt" env \
  BEADS_DOLT_PASSWORD=fixture-secret "$BD_BIN" -C "$TLS/direct-client" show "$tls_issue" --json)"
BEADS_DOLT_PASSWORD=rotated-secret \
  bd_at "$TLS/direct-client" show "$tls_issue" --json >/dev/null

remote_after_rotation_id="$(create_issue "$TLS/remote-source" 'post-rotation remote update')"
remote_revoked_status="$(expect_failure "$RESULTS/remote-revoked-password.txt" env \
  DOLT_REMOTE_USER=compat DOLT_REMOTE_PASSWORD=fixture-secret "$BD_BIN" \
  -C "$TLS/remote-source" dolt push --remote authority)"
DOLT_REMOTE_USER=compat DOLT_REMOTE_PASSWORD=rotated-secret \
  bd_at "$TLS/remote-source" dolt push --remote authority >/dev/null

mkdir -p "$TLS/remote-fresh"
(
  cd "$TLS/remote-fresh"
  DOLT_REMOTE_USER=compat DOLT_REMOTE_PASSWORD=rotated-secret \
    "$BD_BIN" init --remote "http://127.0.0.1:$REMOTE_PORT/remoteprobe" \
    --skip-hooks --skip-agents --non-interactive >/dev/null
)
bd_at "$TLS/remote-fresh" show "$remote_seed_id" --json >/dev/null
bd_at "$TLS/remote-fresh" show "$remote_update_id" --json >/dev/null
bd_at "$TLS/remote-fresh" show "$remote_after_rotation_id" --json >/dev/null

interrupted_id="$(create_issue "$TLS/remote-source" 'interrupted then retried')"
kill "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=''
interrupted_status="$(expect_failure "$RESULTS/remote-interrupted.txt" env \
  DOLT_REMOTE_USER=compat DOLT_REMOTE_PASSWORD=rotated-secret "$BD_BIN" \
  -C "$TLS/remote-source" dolt push --remote authority)"
bd_at "$TLS/remote-source" show "$interrupted_id" --json >/dev/null

"$DOLT_BIN" sql-server --config "$TLS/server-plain.yaml" >>"$RESULTS/plain-server.log" 2>&1 &
SERVER_PID=$!
wait_for_port 127.0.0.1 "$SQL_PORT" || fail 'Dolt server did not recover after interruption'
wait_for_port 127.0.0.1 "$REMOTE_PORT" || fail 'remotesapi did not recover after interruption'
DOLT_REMOTE_USER=compat DOLT_REMOTE_PASSWORD=rotated-secret \
  bd_at "$TLS/remote-source" dolt push --remote authority >/dev/null
mkdir -p "$TLS/interrupted-fresh"
(
  cd "$TLS/interrupted-fresh"
  DOLT_REMOTE_USER=compat DOLT_REMOTE_PASSWORD=rotated-secret \
    "$BD_BIN" init --remote "http://127.0.0.1:$REMOTE_PORT/remoteprobe" \
    --skip-hooks --skip-agents --non-interactive >/dev/null
)
bd_at "$TLS/interrupted-fresh" show "$interrupted_id" --json >/dev/null

kill "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=''

cat >"$TLS/server-tls.yaml" <<EOF
log_level: info
behavior:
  autocommit: true
listener:
  host: 127.0.0.1
  port: $SQL_PORT
  require_secure_transport: true
  tls_cert: "$TLS/certs/server.crt"
  tls_key: "$TLS/certs/server.key"
data_dir: "$TLS/authority-data"
cfg_dir: "$TLS/authority-data/.doltcfg"
remotesapi:
  port: $REMOTE_PORT
EOF

"$DOLT_BIN" sql-server --config "$TLS/server-tls.yaml" >"$RESULTS/tls-server.log" 2>&1 &
SERVER_PID=$!
wait_for_port 127.0.0.1 "$SQL_PORT" || fail 'TLS Dolt SQL server did not become ready'
wait_for_port 127.0.0.1 "$REMOTE_PORT" || fail 'TLS Dolt remotesapi did not become ready'

tls_untrusted_status="$(expect_failure "$RESULTS/tls-untrusted.txt" env \
  BEADS_DOLT_PASSWORD=rotated-secret BEADS_DOLT_SERVER_TLS=1 "$BD_BIN" \
  -C "$TLS/direct-client" show "$tls_issue" --json)"
tls_plaintext_status="$(expect_failure "$RESULTS/tls-required.txt" env \
  BEADS_DOLT_PASSWORD=rotated-secret "$BD_BIN" \
  -C "$TLS/direct-client" show "$tls_issue" --json)"

bd_at "$TLS/remote-source" dolt remote remove authority >/dev/null
bd_at "$TLS/remote-source" dolt remote add authority \
  "https://127.0.0.1:$REMOTE_PORT/remoteprobe"
tls_remote_untrusted_status="$(expect_failure "$RESULTS/tls-remote-untrusted.txt" env \
  DOLT_REMOTE_USER=compat DOLT_REMOTE_PASSWORD=rotated-secret "$BD_BIN" \
  -C "$TLS/remote-source" dolt pull --remote authority)"

record tls-summary.txt "sql_port=$SQL_PORT
remotesapi_port=$REMOTE_PORT
authenticated_plaintext_direct_success=true
authenticated_plaintext_remote_success=true
fresh_authenticated_remote_recovery=true
interrupted_push_exit=$interrupted_status
retry_after_restart_success=true
private_ca_trusted_tls_success=false
untrusted_tls_exit=$tls_untrusted_status
tls_required_plaintext_exit=$tls_plaintext_status
untrusted_remote_tls_exit=$tls_remote_untrusted_status
wrong_password_exit=$auth_wrong_status
revoked_password_exit=$auth_revoked_status
wrong_remote_password_exit=$remote_wrong_status
revoked_remote_password_exit=$remote_revoked_status
rotated_password_success=true"

kill "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=''

if grep -R -F 'fixture-secret' "$RESULTS" >/dev/null; then
  fail 'retained diagnostic output leaked a fixture password'
fi

log 'complete'
printf 'All compatibility scenarios passed.\n'
printf 'Results: %s\n' "$RESULTS"
