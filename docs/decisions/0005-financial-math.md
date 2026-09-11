# ADR-0005: Financial Math for Claim, Settlement, and Cumulative Release

> The release/cap/excess decision was superseded and implemented in PR #10 under [ADR-0010](0010-permanent-gnk-shares.md) dated 2026-09-07. Below is the historical description of the earlier implementation, rather than current active requirements. Capacity and USDT settlement are unchanged.

- Status: Accepted and implemented
- Date: 2026-09-06
- Base: `origin/main@b00483e7c6028b0c985c4cad501921fb30c80b6d`
- Pinned Gonka source: `379bebced638aeb5e6077bfd51c986f898443832`

## Requirements and Decision Boundary

`marketplace-common` provides pure calculations for capacity, claim entitlement, USDT settlement, and cumulative GNK release. Functions accept values and return either a result or `MathError`; they do not read storage or chain queries, do not validate caller/state, and do not create transfer messages.

Units are encoded in field names: `micro_usdt` for the 6-decimal settlement token and `ngonka` for 9-decimal GNK. Floating point arithmetic is not used. Protocol fee is hardcoded as `150 / 10_000`; a caller cannot pass an arbitrary `fee_bps` into a financial function.

## Inputs and Trust Boundary

Immutable `OfferTerms` stores configured budget and price. `BuyerFunding` is an enum with mutually exclusive variants:

- `NoBuyer`: actual funded budget/capacity and Buyer entitlement are zero; configured offer terms are not modified, and the entire claim belongs to the Host;
- `Funded { actual_funded_budget_micro_usdt }`: actual deposit must strictly match the configured budget, after which effective capacity is derived from actual budget and price.

Capacity, total claim, entitlement, gross, fee, and refund are not accepted as simultaneously independent inputs. They are derived from primary values so that an inconsistent caller cannot produce a negative refund or entitlement not backed by a deposit.

Release accepts total claim, Buyer entitlement, previous counters of both parties, remaining vesting, and available bank balance. Host entitlement and overall `previously_released` are derived from these fields. Previous counters must conform to the same cumulative floor formula; violation yields a typed error.

## Invariants and Rounding

- `total_claim = checked_add(work, reward)`;
- `buyer_entitlement = min(total_claim, effective_capacity)`;
- `host_entitlement = total_claim - buyer_entitlement`;
- `gross = floor(buyer_entitlement * price / 1_000_000_000)`;
- `fee = floor(gross * 150 / 10_000)`;
- `host_net + fee + buyer_refund = actual_funded_budget`;
- `released_total = buyer_released + host_released <= total_claim`;
- neither party exceeds entitlement, and counters are monotonically non-decreasing;
- sum of new deltas does not exceed available balance;
- when `released_total == total_claim`, parties receive their exact entitlement.

All multiplications are performed in `Uint256`. The sum `previously_released + available_balance` is also computed in `Uint256` before capping by the unlock cap, ensuring that an intermediate `Uint128` overflow does not reject a representable result.

Rounding remainder of the cumulative GNK split goes to the Host:

```text
buyer_target = floor(U * buyer_entitlement / total_claim)
host_target  = U - buyer_target
```

Targets are computed from cumulative `U`, rather than separately for each tranche. Therefore, at `U = total_claim`, the Buyer receives exactly their Buyer entitlement, the Host receives the entire remainder, and the final state does not depend on the number of intermediate releases.

## Zero, Dust, and Donations

A confirmed zero total is not treated as an inconsistent claim. A funded Buyer receives a full refund calculation; a no-sale yields zero for all USDT amounts. Release with zero total immediately returns `NoNewRelease` and avoids division by zero.

A liquid donation may increase available balance, but the candidate remains capped at `total - min(total, remaining_vesting)`. A vested donation may increase remaining vesting enough that the new cap drops below what has already been distributed. In this case, A2 returns `NoNewRelease`: distributed counters never decrease and underflow does not occur.

## Alignment with Audited References

### DAO DAO

[Oak report](https://github.com/oak-security/audit-reports/blob/main/DAO%20DAO/2023-03-22%20Audit%20Report%20-%20DAO%20DAO%20Vesting%20and%20Payroll%20Factory%20v1.0.pdf) was directly reviewed. Audit base: `0b5cae57fecbbadb1045f3dc2bb4ad4fe5a98ee8`; scope on page 5: `contracts/external/cw-vesting`, `contracts/external/cw-payroll-factory`, and `packages/cw-wormhole`.

Finding #2 (Major) showed that discrepancy between configured vesting total and received CW20 amount leaves funds in the Factory and may allow a subsequent caller to drain them. Remediation `fadf2e4a2bbcc6363ca06c1e6e7bb0c745f00f99` enforced exact equality. Our `FundedBudgetMismatch` applies the same class of protection at the pure boundary. The convenient post-audit snapshot `0178cf55d358356474e5530cccac6acdccd0d94b` is not considered fully audited.

### Astroport

[Oak report](https://github.com/oak-security/audit-reports/blob/main/Astroport/2023-04-04%20Audit%20Report%20-%20Astroport%20Maker%20and%20Vesting%20Contract%20Updates%20v1.0.pdf) covered only changes to Maker at `1f50cabf6738f6ad57b6ed7b1d56f1276fe6d526` and Vesting at `042b0768951422099f5d77224c320978cbfa92cc` relative to the previous audit base. It is not an audit of our formula or the entire Astroport repository.

Finding #8 noted that a zero native withdrawal should terminate with a clear contract result/error before transfer; the report marks it `Resolved` but does not specify a dedicated remediation commit. A2 returns an explicit `NoNewRelease`. Finding #2 regarding unbounded schedules is inapplicable to constant-time A2 formulas, but confirms the rejection of loops over external collections.

### OroSwap

[Halborn report](https://www.halborn.com/audits/oroswap/cosmwasm-contracts-632648) was reviewed with separated scopes: dedicated pool-initializer base `59f095b…` and primary Factory/Vesting scope `9042989f8fd00b6524b470a5850ec03f8e5a2e4e`, which includes `contracts/vesting`.

HAL-06 regarding fee over-allocation was fixed in `308d8c64b8861524c17c796754091494f530d1a0`; A2 hardcodes 150 bps and validates complete USDT conservation. HAL-10 regarding missing balance validation was fixed in `e44c386b90d390f4482540453a1e6fb32d1f6382`; release is simultaneously capped by unlock cap and available balance. These findings are used as risk classes, not proof of Gonka formulas. No OroSwap/Astroport GPL code was copied.

## Test Verification

- Seven mandatory MVP test cases, including under/exact/overproduction;
- Funded, no-sale, zero total, zero components, one-ngonka, and dust;
- Capacity/gross/fee/cumulative split floor rounding;
- `Uint128::MAX`, claim overflow, conversion overflow, and wide capped sum;
- Mismatched funding and invalid entitlement/release counters;
- Consecutive releases, insufficient liquidity, and repeated calls without new distributable balance;
- Liquid donation, growth of remaining after partial payout, and `remaining > total`;
- Property tests for conservation and exact final state under sequential, direct, and reverse random tranches.

Arithmetic tests prove pure functions only. They do not prove real Gonka claim, streamvesting, spendable bank balance, custom gRPC, or atomic rollback transfer; those remain for A5/A6 contract tests and golden Gonka E2E.

## Residual Limitations and Next Steps

A2 by itself does not implement contract transitions. Exact CW20 funding is implemented separately in [ADR-0006](0006-exact-cw20-funding.md); Lock, Cancel, SettleClaim execute, Release execute, Refund, Expired, and ForwardExcessGnk remain subsequent milestones.
