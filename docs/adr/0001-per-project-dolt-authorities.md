# ADR 0001: Use one authoritative Dolt database per Beads project

- Status: Accepted
- Date: 2026-08-02
- Decision bead: `federated-beads-kfv.1`
- Parent epic: `federated-beads-kfv`

## Context

fbd aggregates independently initialized Beads repositories. Each source has a
project identity, effective issue prefix, and Dolt history. The original central
authority proposal left open whether those independent histories could safely
converge into one Dolt database and branch.

External-CLI experiments showed that distinct issue ids do not solve history or
metadata identity. An independently initialized second project cannot push or
pull into the first project's authority because there is no common ancestor.
Shared seeding fixes ancestry but intentionally gives all clients one project id
and prefix. Branch isolation works only with direct Dolt checkout, push, and
promotion operations outside stock bd.

fbd's current attribution also depends on distinct effective prefixes, so a
single shared tracker would erase a product invariant rather than implement the
desired federation.

## Decision

Select Outcome D from the compatibility spike.

The central service is shared infrastructure, not a shared commit history. Each
Beads project has one independently addressable authoritative Dolt database and
remote. Developers and agents continue using embedded local Beads state and
explicit remote pull/push for that project. fbd remains a read-only aggregation
layer and delegates issue and ready/blocked semantics to bd.

The initial deployment pins bd 1.1.0, Beads public schema 1, and Dolt 2.2.3.
Production uses authenticated HTTPS remotesapi with Web-PKI trust. Direct SQL
server mode is deferred and unsupported as a normal client workflow.

## Alternatives rejected

### One branch for independently initialized projects

Rejected. Push and pull fail because histories have no common ancestor. Force
would replace authority state and violates the no-data-loss requirement.

### One shared-seeded tracker

Rejected for federation. It is a valid collaboration model within one project,
but every client shares one project id and issue prefix. It cannot preserve
independent repository identity or fbd's prefix attribution.

### Per-project work branches merged into one database

Rejected as the initial product workflow. bd 1.1.0 cannot switch branches and
does not publish an untracked checked-out branch through its ordinary push.
Direct Dolt operations and a privileged promotion service would add a second
workflow and conflict policy without improving the read-only federation goal.

### Postgres or an fbd-managed issue store

Rejected. It would create a competing authoritative write path and require fbd
to reimplement Beads semantics and reconciliation.

## Consequences

- Unique project identities, prefixes, and histories are preserved.
- A single Fly primary may host multiple databases on one volume, but backup,
  authorization, observability, and restore checks must address each database.
- Remote non-fast-forward and row conflicts remain visible and operator-owned;
  force-push and silent last-writer-wins are forbidden.
- Dolt-native backup is mandatory because JSONL does not preserve the full
  versioned database.
- Production readiness still depends on the gated Fly acceptance run in
  `federated-beads-kfv.2`.
- Direct-server migration and concurrency remain separate work in
  `federated-beads-kfv.3`.

The full evidence and compatibility contract are in
`docs/architecture/central-dolt-compatibility.md`.
