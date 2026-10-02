# Security policy and invariants

The contracts rely on correct native Gonka routing/claim/vesting semantics and a
compliant configured settlement token. Local model tests do not establish a
running chain's compatibility, runtime safety or release readiness.

## Architecture and authority

1. Every Host/epoch gets a separate Deal address and permanent Factory index.
   Terminal outcomes do not permit re-registration or cross-deal withdrawal.
2. Factory and all Deals must have chain-level `ContractInfo.admin = None`.
   Absence of migration entry points alone does not prevent code replacement.
   Deploy with explicit `--no-admin` and independently query actual instances.
3. Public API/protobuf packages contain wire types only; stateless common code
   contains no storage, authorization, transitions or transfer construction.
   Generated bindings come from pinned checksummed upstream sources.
4. Factory reply accepts only ID 1, exactly one expected typed instantiate
   response, valid child address and unused matching IDs. Pending context cannot
   be overwritten; child/reply failure atomically rolls back registry and child.
5. Configuration and funding identity are immutable. Every instantiate/execute
   rejects native funds. Permissionless operations use stored amounts/recipients,
   never caller-selected evidence, routes or destinations.

## Query and lifecycle boundaries

6. Only four compile-time Gonka gRPC routes are used. Responses are at most 32 KiB;
   recipient lists at most 64 entries. Duplicate target rows, invalid identities,
   decoding and validation errors reject routing operations.
7. Funding requires configured CW20 Send, exact budget, Open before E and exact
   Host/E routing to this Deal. Authenticate the immediate token sender before
   trusting hook Buyer identity. Rejected hooks roll back the token transfer.
8. Lock requires Open/Funded, E <= current < E+5 and positive exact routing.
   Success persists recipient proof; it does not move assets.
9. Cancel requires Open without Buyer and validated missing/mismatched routing,
   including before E. Before E only Host may call; E..E+4 anyone may call.
   Query errors and pruned history are never proof of absence.
10. SettleClaim requires Locked, stored proof and exact validated claimed=true
    Host/E summary. It has no routing-pruning deadline and does not re-query
    routing. Missing/error/claimed=false summaries are not zero claims.
11. Funded routing Refund requires E..E+4 and validated missing/mismatch.
    Locked claim-expiry Refund requires stored proof, pristine accounting,
    current >= E+2 and exact claimed=false summary. Claimed=true always rejects
    Refund, regardless of amount or lateness.
12. Only from E+3, explicitly classified summary response/availability failures
    permit NetworkUnconfirmed. No node error text is parsed. Request/current-epoch,
    unexpected system, configuration, accounting and overflow errors remain
    fail-closed. This fallback does not prove absent claims: Buyer receives the
    deposit, Host receives all GNK and no USDT, fee receives no USDT.
13. Refund/expiry outcomes are final even after native recovery. A new retry
    rechecks current evidence only while unfinalized. No transition is automatic;
    external transactions are required.

## Accounting and delivery

14. Financial arithmetic is checked integer math with documented floor rounding.
    Total claim is earned + rewarded; original GNK entitlements sum to claim.
    Host net + fee + Buyer refund equals the exact accepted USDT budget.
    Fee is fixed at 150 bps; token decimals must be 6.
15. Live CW20 balance/donations never change original capacity or obligations.
    Settlement records accrued amounts and per-role pending debts without sending
    funds. Zero confirmed claim completes and accrues full funded Buyer refund;
    no-sale creates no USDT debt.
16. WithdrawUsdt clears only its selected role before one immutable-recipient
    transfer; failure rolls back that transaction and preserves its debt.
    Previously confirmed separate transactions stay committed. Coincident
    addresses do not merge roles; Completed does not extinguish debts.
17. Refund keeps a single atomic Buyer transfer of the configured budget, after
    storing terminal reason/accounting and HostOnly policy. Failure rolls back
    all state and transfer; no-Buyer Expired creates no zero transfer.
18. GNK distribution freezes B/T for positive settlement, or HostOnly 0/1 for
    zero/refund/expiry. Canonical cumulative counters are monotonic, sum to lifetime
    paid and can exceed initial claim. Every release distributes the full ngonka
    bank balance; remaining vesting does not cap it.
19. GNK state is saved before non-zero Bank sends; any send failure rolls back all
    sends/counters in that transaction. Completed occurs once at U >= T but keeps
    late release available. ForwardExcessGnk is Completed-only and preserves the
    same shares; it cannot bypass a positive Buyer share.
20. Release, settlement and refunds do not withdraw unrelated CW20/native denoms.
    Unsupported assets and surplus USDT may remain locked. There is no arbitrary
    administrative recovery. Global token restrictions can still block delivery.

Exact formulas and guards are in [contract behavior](docs/contract-behavior.md);
the emergency error matrix and native assumptions are in
[chain requirements](docs/chain-requirements.md).

## Runtime and release gates

Production needs fresh exact-source native acceptance, reproducible deployable
Wasm and independent deployment queries. Source checkout identity is not running
binary proof; a sync broadcast hash is not included success. Receipts block
duplicate deployments and ambiguous broadcasts are never automatically resent.

Required reports cover four-route allowlisting, native gas/pruning bounds,
runtime/claimed safety, full golden E2E and production parameters/operations.
Verify actual wasmd/wasmvm identity and all applicable advisories, including the
known risk of the previously reviewed wasmd v0.54.2. Native constrained-gas/FFI
tests must establish that healthy queries cannot become successful emergency
refunds. Contract size checks cannot bound native response construction.

The pinned source/runtime model expects recipient immutability from E, potential
pruning from E+5, a closed claim window by E+2, truthful retained summaries and
spendable bank credits after streamvesting unlock. These must be checked on the
selected chain. A defensive claimed-zero settlement is not proof that the native
runtime can produce it. Contract tests and external audited reference patterns do
not constitute a security audit of Marketplace or Gonka.

See [release guide](docs/release-guide.md) and [validation](docs/validation.md)
for evidence bindings, required scenarios and model limits.

## Reporting a vulnerability

Do not disclose suspected vulnerabilities publicly or through public issues.
Report privately to `support@gonka24.com` with subject
`Security: Gonka24 Marketplace`, affected version/commit, reproduction steps,
contracts and potential impact.

When enabled, GitHub Private Vulnerability Reporting under
Security > Advisories > Report a vulnerability is also available.
