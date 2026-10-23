# rchain-observer

A read-only observation service that compares finalized snapshots across configured RChain HTTP endpoints and checks bounded raw-block justification ancestry when heights differ.

Initiated and developed by [AixAria (`@aixaria0`)](https://github.com/aixaria0) for the [RChain Community](https://github.com/rchain-community).

[Rholox](https://rholox.com) · [Hosted observation API](https://testnet.rhobot.net/observer/v1/observation) · [Hosted health](https://testnet.rhobot.net/observer/health) · [DApp guide](docs/BUILDING-DAPPS.md)

The hosted deployment can run a different revision from this repository. Inspect `/health` and its `version` field before assuming that a source change has been deployed. Older deployments do not expose that field.

## Trust boundary

The service answers **what the configured endpoints reported, whether their finalized identities match, and whether bounded raw justification paths connect different reported heights**.

It does not independently run Casper, execute blocks, authenticate the network ID, validate a validator bond set or stake quorum, or verify a consensus finality certificate. A valid block signature establishes possession of its proposer key; membership and authorization of that key are separate questions. Network and finalized status originate from node HTTP responses. Raw block hashes and proposer signatures are checked only for traversed ancestry evidence, not for every matching snapshot.

Endpoint diversity is operator configuration. Different labels and URLs do not prove independently controlled nodes. Agreement is a comparison of available evidence, not an authorization decision. In particular, **one comparable node can yield `agreement` while coverage remains insufficient**. Preserve the observed/comparable/reachable denominators.

The observer holds no signing keys and submits no deploys, proposals or admin mutations. Applications remain responsible for their own execution, authorization and risk policies.

## Run

```bash
cargo build --release --locked

./target/release/rchain-observer \
  --node A=http://127.0.0.1:40403 \
  --node B=http://127.0.0.1:41403 \
  once

./target/release/rchain-observer \
  --node A=http://127.0.0.1:40403 \
  --node B=http://127.0.0.1:41403 \
  --timeout-ms 2500 \
  --ancestry-max-blocks 512 \
  serve --listen 127.0.0.1:8088 --refresh-ms 5000 --stale-after-ms 30000
```

Configure 1–32 targets. Names contain 1–64 letters, digits, dots, underscores or hyphens. Names and normalized URLs must be unique. URLs must be absolute HTTP(S) addresses without credentials, queries or fragments. Path prefixes are supported. HTTP redirects are refused; configure the intended final endpoint directly. URL aliases and shared operators cannot be detected by configuration validation.

The timeout is 1–30,000 ms. The CLI rejects ancestry budgets outside 1–4096; the library clamps larger budgets to 4096, and a zero library budget leaves ancestry unresolved. Refresh and stale thresholds must each be at least 1000 ms. Select a freshness threshold that accommodates the deployment's polling and network budgets.

## HTTP contract

| Route | HTTP behavior | Meaning |
|---|---|---|
| `GET /v1/observation` | 200, latest cached JSON report | Evidence, including incomplete/unavailable/conflicting results |
| `GET /health` | 200 when the HTTP process responds | Liveness; payload separately reports `ok`, `degraded` or `stale` |
| `GET /ready` | 200 or 503 | Fresh, complete coverage of at least two configured endpoints, with `agreement` or `proven_lag` |

Readiness is a defined monitoring policy, not a consensus certificate. It does not require finalized heights to be identical when ancestry has connected them. HTTP snapshot requests never trigger validator fan-out. GET routes also support HEAD; other methods are rejected. Unknown routes return 404. Responses include `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`, a restrictive content-security policy and credential-free public read CORS.

Example report, using illustrative heights, timestamps and hashes:

```json
{
  "schema_version": 1,
  "observed_at_ms": 1792690000000,
  "completed_at_ms": 1792690000042,
  "duration_ms": 42,
  "heights": {
    "reachable_nodes": 2,
    "latest_min": 102,
    "latest_max": 102,
    "finalized_min": 100,
    "finalized_max": 100
  },
  "observations": [
    {
      "node": "A",
      "reachable": true,
      "network_id": "testnet",
      "shard_id": "/root",
      "latest_block": 102,
      "finalized_block": 100,
      "finalized_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "error": null
    },
    {
      "node": "B",
      "reachable": true,
      "network_id": "testnet",
      "shard_id": "/root",
      "latest_block": 102,
      "finalized_block": 100,
      "finalized_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "error": null
    }
  ],
  "ancestry_checks": [],
  "verdict": {
    "class": "agreement",
    "observed_nodes": 2,
    "comparable_nodes": 2,
    "agreeing_nodes": 2,
    "summary": "2/2 comparable nodes agree",
    "reference": {
      "network_id": "testnet",
      "shard_id": "/root",
      "block_number": 100,
      "block_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    }
  }
}
```

Real reports retain every configured row, including failed or incomplete observations.

`observed_at_ms` is the **start of the sampling window**, not the time the final request finished. `completed_at_ms` is its end. Requests within one cycle are concurrent, but the sample is not an atomic network snapshot. `duration_ms` and service freshness use monotonic time, so a wall-clock adjustment cannot make an old cached report ready. JSON timestamps use Unix milliseconds; consumers should reject stale/future-clock evidence according to their policy.

`/health` includes `version`, `ready`, `stale`, `snapshotAgeMs`, `staleAfterMs`, sampling timestamps/duration, observed/comparable/reachable counts, verdict and ancestry-check count. Its age is measured from sample start.

Internal target URLs are omitted from serialized observations. Public failures contain bounded error categories or HTTP status codes, not upstream URLs or response bodies. Node labels and returned public network/shard identifiers remain visible; choose labels suitable for publication.

## Verdict semantics

A comparable identity is `(network ID, shard ID, finalized height, 32-byte finalized hash)`. Failed or incomplete rows cannot vote. Empty identities and malformed hashes are excluded; valid hexadecimal hashes are normalized to lowercase. The public classifier excludes repeated node labels. Normal observer construction rejects duplicate labels or normalized URLs before sampling.

| Verdict | What the evidence establishes |
|---|---|
| `agreement` | All **comparable** snapshots have one identical finalized identity. Check coverage separately. |
| `height_skew` | Same network/shard, different heights, and no same-height hash conflict; ancestry is unresolved. |
| `proven_lag` | Every distinct lower finalized identity was reached by a verified raw justification path from the highest reference. |
| `majority` | A strict count majority has one finalized identity, with an observed same-height hash conflict. This is not stake-weighted consensus. |
| `conflicting_finalized_history` | A complete bounded traversal of the supplied higher raw history did not connect to a reported lower finalized identity. This is a non-connection finding, not an independently verified Casper safety violation. |
| `divergence` | Network/shard identities differ, or same-height hashes conflict without a strict count majority. |
| `unavailable` | No comparable finalized identity was returned. |

The highest height selects the ancestry reference. When a same-height conflict is present, the most-supported identity selects the majority reference; an isolated higher tip cannot hide a lower strict majority. Tie-breaking uses a deterministic total ordering.

The two snapshot endpoints are `/api/status` and `/api/last-finalized-block`. If both return shard IDs, they must match. HTTP reachability remains distinct from identity completeness: a node can respond successfully with insufficient evidence, stay visible, and be excluded from comparison.

## Raw ancestry verification and resource bounds

Traversal fetches `GET /api/v1/block/{hash}/raw`. For each block it:

1. Validates the requested/returned 32-byte content address.
2. Hashes the encoded preimage with BLAKE2b-256 and compares it locally.
3. Decodes the relevant protobuf fields only after that hash matches.
4. Checks the cleared block-hash/signature fields and binds the detached envelope's sender and algorithm to the preimage.
5. Verifies the detached `secp256k1` DER or `secp256k1:eth` compact signature over the block hash.
6. Requires the expected shard, heights and descending justification edges.

A found path establishes ancestry in that committed justification graph. Missing, malformed, unsupported, oversized, timed-out or budget-exhausted evidence stays unresolved. The observer does not infer ancestry merely from height order.

| Resource | Bound |
|---|---|
| Status/finalized JSON body | 64 KiB per response, including chunked bodies |
| Raw-block JSON body | 4 MiB per response, including chunked bodies |
| Configured endpoints | 32 |
| Unique fetched **or queued** ancestry hashes | Per-check block budget, at most 4096 |
| Justifications in one raw block | 1024 |
| All ancestry pairs and witness fallbacks | One shared deadline of twice the configured HTTP timeout per cycle |

Requests, including body reads, have a total HTTP timeout. The traversal stores one predecessor per unique hash rather than copying a complete path for every frontier entry. Duplicate edges do not repeatedly queue a block. Valid blocks beyond the operational size/fan-out limits are conservatively unresolved. The limits are observation policy, not protocol validity limits.

Polling never overlaps itself; missed interval ticks are skipped. SIGINT/SIGTERM initiates graceful HTTP shutdown. There is no historical persistence or signed observer-report provenance in this version.

## Deployment

`deploy/rchain-observer-testnet.service` and `deploy/nginx-observer.conf` provide a loopback-only, dedicated-account service and read-only reverse proxy. Keep private target configuration on the host. Choose memory/time limits appropriate to the configured topology. Changing this repository does not authorize changing validator configuration, bonds or consensus behavior.

Deploy a reviewed release artifact, retain the previous binary, restart only the observer, and verify `/health`, `/ready` and a complete observation report. A 503 readiness result is evidence to investigate; do not replace it with an unconditional success response. The supplied systemd example uses four endpoints as an operator configuration, not a hard-coded requirement.

Verify the public HTTPS path from the consuming browser as well as loopback health. The service emits CORS and cache headers itself. A proxy that also adds them must hide the upstream copies first: browsers reject multiple `Access-Control-Allow-Origin` values, even when both are `*`. The supplied Nginx example keeps one edge-owned copy of CORS, cache and content-type protection headers, including on error responses, and preserves upstream readiness status codes.

With Nginx and Python 3 installed, `python3 scripts/test-nginx-proxy.py` exercises that exact proxy example against an isolated mock upstream. It checks single response headers, GET/HEAD behavior, propagation of readiness failures, and blocked writes. It uses temporary configuration and ephemeral loopback ports; it does not reload the system Nginx service or contact a validator. CI runs this contract separately from Rust tests.

## Reproducible verification

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
cargo build --release --locked --target x86_64-unknown-linux-musl
cargo audit
```

Install the musl target/toolchain and `cargo-audit` before their checks. CI runs formatting, Clippy, deterministic local HTTP/cryptographic/classifier tests, dependency audit, and the static Linux release build on pull requests. Tests use isolated mock endpoints and require no shared testnet mutations.

See [the assurance matrix](docs/ASSURANCE.md) for invariant coverage, residual trust and review limits. [Rholox](https://github.com/aixaria0/Rholox) consumes this API, validates report consistency, preserves per-source rows, and distinguishes stale/limited/conflicting evidence in its UI. [Building DApps](docs/BUILDING-DAPPS.md) documents the separate execution path.
