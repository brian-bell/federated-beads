# Fly.io single-primary Dolt operational acceptance

This is the gated acceptance suite for `federated-beads-kfv.2`. It is separate
from ordinary `cargo test` and the local compatibility harness because it needs
credentials, DNS/certificates, a persistent volume, and billable infrastructure.
No deployment is accepted until every assertion below has retained redacted
evidence.

## Fixed inputs

- A reviewed Dolt SQL-server image pinned by immutable digest and reporting
  Dolt 2.2.3.
- One Fly app, one primary machine, one region, and one persistent volume. No
  multi-primary, automatic failover, or cross-region write claim.
- Stable database names for at least two independent fixture projects.
- A Web-PKI certificate for the public remotesapi endpoint and authenticated
  client credentials supplied only through Fly secrets and ephemeral client
  environment variables.
- An off-machine, encrypted backup destination outside the app and its volume.
- Named operator/approver plus explicit target RPO and RTO.

The runner must refuse floating image tags, unencrypted public endpoints,
missing cleanup metadata, absent backup destination, or use of production
project data.

## Evidence bundle

Create a run-id directory outside the repository containing:

- image digest, Fly CLI version, bd/schema/Dolt versions, region, machine id,
  volume id, database names, and certificate issuer/expiry (never private key);
- redacted commands, UTC timestamps, exit codes, stdout/stderr, readiness
  responses, and server logs;
- before/after Dolt history ids for each write, failed write, restart, backup,
  restore, upgrade, rollback, and portability step;
- issue/dependency/status/branch/history manifests from fresh clients;
- backup checksum, encryption metadata, restore duration, observed RPO/RTO, and
  cleanup results.

Automated redaction must scan for fixture passwords, tokens, private-key
headers, authorization headers, and Fly secret values before the bundle can be
retained.

## Acceptance sequence

### 1. Provision and readiness

1. Create the app, volume, secret set, certificate/DNS, and one primary from
   reviewed configuration.
2. Mount the volume at the configured Dolt data directory; fail if the server
   can start against ephemeral image storage instead.
3. Readiness must prove the process is serving, the volume is writable, both
   databases are enumerated, authenticated SQL diagnostics work, and HTTPS
   remotesapi can perform an authenticated read. A TCP connect alone is not
   sufficient.

Pass: the pinned server becomes ready within the declared startup budget and no
credential or key appears in deploy output or logs.

### 2. Project isolation and client workflow

1. Bootstrap two projects with different project ids/prefixes into separate
   database/remotes.
2. From two clients per project, exercise serial changes, concurrent
   non-conflicting changes, non-fast-forward pull/merge/retry, an explicit
   same-row conflict, idempotent no-change sync, and fresh-client recovery.
3. Attempt cross-project reads and writes with each project's client
   credentials.

Pass: each project converges internally, conflict failures preserve both sides,
and no project can enumerate, read, push, or overwrite another project's data.
No force operation is used.

### 3. Authentication and TLS

1. Verify authenticated HTTPS success through the exact stock bd remote-sync
   commands in the compatibility contract.
2. Verify missing/wrong password, revoked user, expired token if applicable,
   and cross-project credentials fail without authority history changes.
3. Rotate credentials; prove old credentials fail and new credentials work for
   existing and fresh clients.
4. Verify plaintext, wrong hostname, untrusted certificate, and expired/invalid
   certificate fail. Renew/replace the Web-PKI certificate and prove clients
   recover without disabling verification.

Pass: every positive request is encrypted and authenticated; every negative
case is explicit and non-mutating. Private CA and mTLS are not accepted as
substitutes unless a later bd compatibility contract adds them.

### 4. Restart and redeploy durability

1. Record database and history manifests, stop/restart the machine, and rerun
   readiness plus fresh-client verification.
2. Deploy the exact same image/config as a replacement machine attached to the
   retained volume and repeat verification.
3. Interrupt a client push while the machine is unavailable, confirm local
   state remains, restore service, and retry.

Pass: volume-backed state and history survive, no empty shadow authority is
created, and interruption/retry is safe.

### 5. Backup and disaster restore

1. Create a Dolt-native off-machine backup for every project, record checksums,
   and validate integrity.
2. Isolate the original app and volume so the restore cannot accidentally read
   them.
3. Restore into a clean replacement app/volume and configure fresh credentials
   and certificates.
4. Verify project ids, prefixes, issue fields, dependencies, statuses, branches,
   working sets where applicable, and exact authoritative history from fresh
   clients.

Pass: restored data meets the approved RPO/RTO and is independently usable. An
untested backup does not pass.

### 6. Upgrade and rollback

1. Take and verify a backup.
2. Run the local full compatibility matrix against the proposed image/version.
3. Upgrade the Fly primary, rerun critical client and fresh-recovery assertions,
   then execute the documented rollback using the verified pre-upgrade data.

Pass: both upgrade and rollback preserve the contract. Any unlisted bd/schema/
Dolt combination is rejected before a client write.

### 7. Provider portability

1. Restore the same backup into a clean, non-Fly self-managed Dolt 2.2.3
   target with new provider-neutral credentials and Web-PKI TLS.
2. Repoint clean clients using only remote configuration changes and verify the
   full state/history manifest.

Pass: no Fly control-plane service or proprietary data conversion is required
to recover the authority.

## Cleanup and failure behavior

On success or failure, revoke fixture credentials, remove certificates and DNS
records, stop/delete machines, and delete volumes only after the off-machine
evidence and requested backup retention are confirmed. Record resources that
could not be removed and the exact non-interactive cleanup command. Never delete
an ambiguous app, volume, certificate, or backup target.

Any failed assertion leaves `federated-beads-kfv.2` open. The parent epic may
proceed to implementation work after the topology decision, but production
deployment is blocked until this suite passes.
