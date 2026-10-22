# Observer assurance matrix

This document describes implementation checks, not formal verification or an independent security certification.

| Invariant | Executable evidence |
|---|---|
| Failed/incomplete identities do not vote | `malformed_or_failed_rows_never_vote`, `snapshot_identity_validation_and_topology_redaction` |
| Duplicate labels cannot amplify a report | `repeated_labels_are_excluded_and_hex_case_is_canonical`, configuration rejection tests |
| Hash letter case does not create a false conflict | Classifier and HTTP identity tests |
| A higher outlier cannot hide a lower strict majority | `highest_height_cannot_hide_a_lower_strict_majority` with input permutations |
| Reference ties are deterministic across identity domains | `reference_ties_have_a_total_deterministic_order` |
| Status and finalized shard disagreement is rejected | Mock HTTP identity test |
| Upstream response bodies cannot grow without a bound | Chunked-body size test and fixed limits in `src/http.rs` |
| A body stalled after headers is timed out | `body_timeout_covers_the_entire_read` |
| Redirects/private upstream error bodies are not published | `redirect_and_private_error_bodies_are_not_followed_or_published` |
| Reachability, readiness and freshness remain distinct | `health_readiness_freshness_and_http_boundary_are_distinct` |
| Observer HTTP paths cannot proxy writes | POST rejection and unknown node-route tests |
| Raw content and proposer signatures are verified | Existing tamper, sender mismatch, invalid-signature and both-algorithm fixtures in `tests/ancestry.rs` |
| Ancestry absence or missing evidence does not become assumed lag | Existing conflict/missing-evidence fixtures |
| Frontier allocation is bounded before fetch | Wide-frontier test |
| Duplicate edges fetch one block and preserve the path | Duplicate-justification test |
| A single raw block cannot create unbounded fan-out | Excessive-justification test |

Run the complete locked test suite; individual fixtures are not a substitute for the full run. Static Linux artifact compilation checks portability separately from host tests. Dependency audit results describe the advisory database at scan time and are not a guarantee against unknown vulnerabilities.

## Residual trust and limitations

- A coordinated or compromised set of configured endpoints can agree on a false finalized snapshot. The observer has no separately authenticated validator set, consensus replay or finality certificate.
- Proposer-key validity is not validator membership. Raw preimage hashing and signature checks protect the ancestry fields being traversed; they do not certify the chain's execution or reported finality.
- Sampling is a time window. Concurrent endpoint requests can observe different moments; a transient split is not independently classified as a protocol safety failure.
- Different endpoints can share an operator, machine or upstream data source. URL/name uniqueness is configuration hygiene, not a Sybil defense.
- Reports are cached in memory and unsigned. There is no durable audit log, observer-instance quorum, rollback/reorg history, or externally pinned report provenance.
- Operational response, edge, block and deadline limits can leave a valid long/large history unresolved. Failure to prove ancestry within a budget is not conflicting-history evidence.
- Readiness is intentionally stricter than snapshot classification. Single-source agreement and partial coverage remain degraded even when their comparable subset matches.
- Local deterministic tests and CI builds do not establish behavior under every live network fault or replace independent review.

No validator bonds, keys, testnet consensus parameters or shared chain state are changed by these verification commands.
