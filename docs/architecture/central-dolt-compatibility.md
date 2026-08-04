# Central Dolt compatibility contract

Status: accepted on 2026-08-02 for `federated-beads-kfv.1`.

The selected result is **Outcome D**: a single Dolt service may host several
projects, but every Beads project has its own authoritative database/remote and
its own commit history. fbd continues to aggregate those projects read-only.
Unrelated Beads repositories must not be merged into one Dolt history.

This is a topology compatibility decision, not a claim that the Fly.io service
is production-ready. The credentialed deployment gate is tracked separately by
`federated-beads-kfv.2`.

## Reproducible evidence

The gated harnesses drive public `bd` and `dolt` commands in isolated fixture
roots. They do not use fbd mocks, the live repository's Beads database, normal
HOME/XDG state, or force-pushes.

```bash
# Complete topology, conflict, auth, TLS-negative, interruption, and recovery matrix
BD_BIN=/path/to/bd DOLT_BIN=/path/to/dolt make test-central-dolt

# One external-server version combination
BD_BIN=/path/to/bd DOLT_BIN=/path/to/dolt \
  EXPECT=compatible make test-central-dolt-version

# Deliberately unsupported version
BD_BIN=/path/to/bd DOLT_BIN=/path/to/old-dolt \
  EXPECT=incompatible make test-central-dolt-version
```

Set `KEEP_COMPAT_FIXTURE=1` on the full matrix to retain its redacted results
directory. By default all repositories, credentials, certificates, ports,
processes, and logs are removed. The harness fails if its retained diagnostics
contain the fixture password.

The completed run used:

| Component | Version | Artifact integrity |
| --- | --- | --- |
| bd | 1.1.0, build `8e4e59d39` | Installed local executable |
| Beads public schema | 1 | Reported by `bd context --json` |
| Dolt full matrix | 2.2.3 | Official Darwin arm64 release, SHA-256 `8adad74935061f1353907843e4c1a926f88ef96dc9d233824a36cd292cb7aa3f` |
| Dolt adjacent server probe | 2.2.2 | Official Darwin arm64 release, SHA-256 `13feab5b3ad36f365cf3a5eb0a45de8bfd9b54268daa8ed705c37acb511fad4a` |
| Dolt unsupported probe | 1.75.0 | Official Darwin arm64 release, SHA-256 `982ae703ce0b3843eeceee5f8c8c2860b0703016203f7d9046134337517faf12` |

No system Dolt, Docker, or Podman installation was used. Release archives were
downloaded to a disposable directory and verified before execution.

## Results matrix

| Scenario | Expected | Actual evidence | Decision |
| --- | --- | --- | --- |
| A: independent `alpha` and `beta` histories push to one branch | Safe convergence or safe rejection | Alpha's first push succeeded. Beta push and pull exited 1 with “no common ancestor.” Authority HEAD was identical before and after the rejected push (`sr2q…e3qm` in the retained sample), and both local records remained intact. | Reject A. Distinct issue prefixes do not create Dolt ancestry. |
| B: two clients cloned from one shared seed | Serial and non-conflicting collaboration works | Clients shared one project id and `shared-` issue prefix. Issues, a blocking dependency, ready/blocked results, and fresh-client reads converged. A divergent push failed non-fast-forward, then pull/merge/retry succeeded. An idempotent pull/push left authority HEAD unchanged (`ak2s…qa48`). | Supported only as collaboration inside one project tracker. It is not cross-project federation. |
| B: same-record divergent edits | Safe explicit conflict | Second push failed non-fast-forward. Pull exited 1 with conflicts in `issues` and `metadata`, restored the working set, and preserved the local title for operator resolution. | No automatic conflict resolution. |
| C: isolated work branch | Stock-bd branch publication | `bd branch` created a branch, but has no switch operation. After direct Dolt checkout, `bd dolt push` reported success without publishing the non-upstream branch. Direct `dolt push`, promotion merge, and main push made the issue visible to a fresh bd client. | Reject C as a supported stock-bd workflow. |
| D: two projects, two remote databases | Preserve independent identity and prevent cross-read/overwrite | Fresh clients retained different project ids and `alpha-`/`beta-` prefixes, recovered their own issues, and could not read the other project's ids. | Select D. |
| Backup and restore | Preserve more than exported issue rows | `bd backup sync` plus clean restore preserved project id, issue state, the `retained-branch`, and the exact main history head (`cdqe…3sgt`). | Dolt-native backup is required; JSONL export is not backup. |
| Auth failure and credential rotation | Reject bad/revoked credentials safely | Direct SQL and remotesapi writes succeeded with the valid fixture user. Wrong and old passwords exited 1 with access denied. After `ALTER USER`, the new password worked and a fresh authenticated client recovered all remote records. | Separate SQL and remote credential surfaces; rotation must test both. |
| Interrupted remote push | Preserve local work and permit retry | With the authority stopped, push exited nonzero and the new local issue remained readable. Restarting the same authority and retrying succeeded; a fresh client recovered the issue. | Retry is supported after reachability recovery. |
| TLS required but client uses plaintext | Safe failure | Direct SQL exited 1: server does not allow insecure connections. | TLS is mandatory outside a local fixture boundary. |
| Private/untrusted CA | Safe failure | Direct SQL and HTTPS remotesapi both exited 1 with certificate verification errors. No authority state changed. | Private-CA trust is not part of the bd 1.1.0 contract. Use Web-PKI certificates. |
| Dolt 2.2.3 external server | Compatible | `bd init`, schema creation, create, show, and server-mode context passed. | Full-matrix pin. |
| Dolt 2.2.2 external server | Compatible smoke test | The same init/create/show/context probe passed. | Adjacent compatible candidate, not the initial deployment pin. |
| Dolt 1.75.0 external server | Safe incompatibility | `bd init` exited 1 during `schema_migrations` inspection with “table has unknown fields”; no usable Beads database was accepted. | Unsupported. |
| Dolt server `dolt_transaction_commit: true` | Compatible with bd-managed commits | Initialization retried “nothing to commit” and eventually failed a migration with a duplicate key. | Forbidden server configuration for this contract. Keep the setting false/default. |

The private-CA probe is deliberately negative. bd 1.1.0 enables direct SQL
TLS through `BEADS_DOLT_SERVER_TLS=1` and the Go MySQL client's standard trust
store; it does not expose a supported custom-CA or direct-server mTLS contract.
The local harness cannot mint a Web-PKI certificate for localhost without
altering machine trust. Healthy authenticated Web-PKI TLS is therefore an
explicit assertion in the credentialed Fly gate, not a result inferred from a
self-signed certificate. This is the upstream boundary that permits the spike's
minimal executable prototype while preventing a false production-readiness
claim.

## Supported initial contract

### Authority layout

- Operate one logical Dolt database and remote history per Beads project.
- A single-primary server process and persistent volume may host several of
  those databases, but database names, branches, histories, users/grants, and
  backups must remain independently addressable.
- Keep every project's existing Beads `project_id` and effective issue prefix.
  Stable distinct prefixes are required by fbd's source attribution.
- Dolt is the only authoritative write store. Postgres, JSONL, and the fbd hub
  are non-authoritative views or interchange artifacts.

### Client workflow

- The supported initial client mode is embedded/local Beads with an explicit
  remote for only that project.
- Bootstrap new clients from the authoritative project remote; do not run an
  independent `bd init` and attempt to merge its history later.
- Pull before push. On a non-fast-forward result, pull, inspect/resolve any
  conflict, and retry. Never automate `--force` as a recovery path.
- An unreachable authority must leave local state usable. Retry only after the
  same authority is healthy or after an operator-approved restore/failover.
- Same-row conflicts require an operator decision. No last-writer-wins or
  automatic `ours`/`theirs` policy is supported.

### Protocol, authentication, and TLS

- Remote-sync uses the Dolt remotesapi HTTP(S) surface. SQL-server credentials
  and remote credentials are separate operational planes.
- Pass remote authentication through `DOLT_REMOTE_USER` and
  `DOLT_REMOTE_PASSWORD`; pass direct SQL authentication through
  `BEADS_DOLT_SERVER_USER`/`BEADS_DOLT_PASSWORD` if running a diagnostic probe.
  Do not persist passwords in repository config or evidence.
- Production remotes must use HTTPS with a certificate chaining to the client's
  standard Web-PKI roots. Plain HTTP is allowed only inside the isolated local
  compatibility harness.
- Private CAs, direct-server mTLS, and certificate pinning are unsupported by
  this initial bd 1.1.0 contract.

### Version policy

- Initial production pin: bd 1.1.0, Beads public schema 1, Dolt server/image
  2.2.3.
- Dolt 2.2.2 passed the external-server smoke probe but is not a production pin
  until the full topology/auth/recovery matrix runs against it.
- Dolt 1.75.0 and all unlisted combinations are unsupported. Reject them before
  a client write where possible and restore the pinned version if discovered.
- Run the full matrix before changing bd, public schema, Dolt server, or Dolt
  image. Take and verify a backup before any accepted upgrade.

### Operations and recovery

- Use Dolt-native backup/restore, which preserves branches, history, and working
  sets. `bd export`/JSONL is not a disaster-recovery artifact.
- Readiness must verify process, storage, database enumeration, authenticated
  access, and the remotesapi endpoint; an open TCP port alone is insufficient.
- Retained diagnostics must include redacted commands, versions, exit codes,
  authority history before/after, and server logs. They must never contain
  passwords, private keys, or bearer material.

## Rejected alternatives and deferred work

- A is unsafe because unrelated histories have no merge base.
- B is valid within one shared tracker but destroys independent project
  identity/prefix attribution, so it cannot implement fbd federation.
- C depends on direct Dolt branch control and an external promoter that stock bd
  does not provide.
- Direct-server mode is not a “low-cost switch” yet. The failed
  `dolt_transaction_commit` probe and missing custom-CA/mTLS surface make its
  migration, concurrent-write, mixed-mode, and rollback contract a separate
  spike: `federated-beads-kfv.3`.
- The real Fly.io persistence, Web-PKI TLS, secret rotation, restore, upgrade,
  and provider-portability run is `federated-beads-kfv.2` and remains a parent
  epic gate.

Primary upstream references:

- [Beads Dolt backend](https://github.com/gastownhall/beads/blob/main/docs/DOLT.md)
- [Dolt remotes and sql-server remotesapi](https://www.dolthub.com/docs/sql-reference/version-control/remotes/)
- [Dolt remote authentication](https://www.dolthub.com/docs/sql-reference/version-control/remote-authentication/)
- [Dolt server configuration](https://www.dolthub.com/docs/sql-reference/server/configuration/)
