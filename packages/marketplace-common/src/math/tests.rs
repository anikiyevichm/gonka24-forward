use cosmwasm_std::{Uint128, Uint256};
use proptest::prelude::*;

use super::*;

fn whole_gnk(amount: u128) -> Uint128 {
    Uint128::new(amount * GNK_SCALE)
}

fn offer(budget_micro_usdt: u128, price_micro_usdt_per_gnk: u128) -> OfferTerms {
    OfferTerms {
        configured_budget_micro_usdt: Uint128::new(budget_micro_usdt),
        price_micro_usdt_per_gnk: Uint128::new(price_micro_usdt_per_gnk),
    }
}

fn funded(actual_funded_budget_micro_usdt: u128) -> BuyerFunding {
    BuyerFunding::Funded {
        actual_funded_budget_micro_usdt: Uint128::new(actual_funded_budget_micro_usdt),
    }
}

fn released(result: CumulativeReleaseResult) -> CumulativeReleaseAmounts {
    match result {
        CumulativeReleaseResult::Release(amounts) => amounts,
        CumulativeReleaseResult::NoNewRelease => panic!("expected a positive release"),
    }
}

#[test]
fn capacity_uses_floor_rounding() {
    assert_eq!(
        funded_capacity_ngonka(Uint128::new(10), Uint128::new(3)).unwrap(),
        Uint128::new(3_333_333_333)
    );
}

#[test]
fn capacity_rejects_zero_price_and_conversion_overflow() {
    assert_eq!(
        funded_capacity_ngonka(Uint128::one(), Uint128::zero()).unwrap_err(),
        MathError::ZeroPrice
    );
    assert!(matches!(
        funded_capacity_ngonka(Uint128::MAX, Uint128::one()),
        Err(MathError::ArithmeticOverflow { .. })
    ));
}

#[test]
fn all_seven_mvp_economic_examples_match() {
    let examples = [
        (50, 2, 100, 52, 0, 52),
        (90, 10, 100, 100, 0, 100),
        (100, 100, 100, 100, 100, 100),
        (99, 1, 100, 100, 0, 100),
        (10, 90, 100, 100, 0, 100),
        (50_000, 50_000, 4_000, 4_000, 96_000, 4_000),
        (0, 0, 100, 0, 0, 0),
    ];

    for (reward, work, capacity, buyer, host, gross_usdt) in examples {
        let budget_micro_usdt = capacity * 1_000_000;
        let result = calculate_claim_settlement(
            offer(budget_micro_usdt, 1_000_000),
            funded(budget_micro_usdt),
            whole_gnk(work),
            whole_gnk(reward),
        )
        .unwrap();

        assert_eq!(
            result.entitlements.effective_funded_capacity_ngonka,
            whole_gnk(capacity)
        );
        assert_eq!(
            result.entitlements.buyer_entitlement_ngonka,
            whole_gnk(buyer)
        );
        assert_eq!(result.entitlements.host_entitlement_ngonka, whole_gnk(host));
        assert_eq!(
            result.usdt.gross_micro_usdt,
            Uint128::new(gross_usdt * 1_000_000)
        );
    }
}

#[test]
fn work_and_reward_mix_does_not_change_same_total() {
    let terms = offer(100_000_000, 1_000_000);
    let reward_heavy =
        calculate_claim_settlement(terms, funded(100_000_000), whole_gnk(1), whole_gnk(99))
            .unwrap();
    let work_heavy =
        calculate_claim_settlement(terms, funded(100_000_000), whole_gnk(90), whole_gnk(10))
            .unwrap();

    assert_eq!(reward_heavy.entitlements.total_claim_ngonka, whole_gnk(100));
    assert_eq!(
        reward_heavy.entitlements.total_claim_ngonka,
        work_heavy.entitlements.total_claim_ngonka
    );
    assert_eq!(
        reward_heavy.entitlements.buyer_entitlement_ngonka,
        work_heavy.entitlements.buyer_entitlement_ngonka
    );
    assert_eq!(
        reward_heavy.entitlements.host_entitlement_ngonka,
        work_heavy.entitlements.host_entitlement_ngonka
    );
    assert_eq!(reward_heavy.usdt, work_heavy.usdt);
}

#[test]
fn no_buyer_keeps_offer_terms_but_assigns_all_claim_to_host() {
    let result = calculate_claim_settlement(
        offer(100_000_000, 1_000_000),
        BuyerFunding::NoBuyer,
        whole_gnk(2),
        whole_gnk(50),
    )
    .unwrap();

    assert_eq!(result.entitlements.total_claim_ngonka, whole_gnk(52));
    assert_eq!(
        result.entitlements.effective_funded_capacity_ngonka,
        Uint128::zero()
    );
    assert_eq!(
        result.entitlements.buyer_entitlement_ngonka,
        Uint128::zero()
    );
    assert_eq!(result.entitlements.host_entitlement_ngonka, whole_gnk(52));
    assert_eq!(
        result.usdt,
        UsdtSettlement {
            actual_funded_budget_micro_usdt: Uint128::zero(),
            gross_micro_usdt: Uint128::zero(),
            fee_micro_usdt: Uint128::zero(),
            host_net_micro_usdt: Uint128::zero(),
            buyer_refund_micro_usdt: Uint128::zero(),
        }
    );
}

#[test]
fn zero_total_refunds_funded_buyer_without_gnk_or_fee() {
    let result = calculate_claim_settlement(
        offer(123_456, 1_000_000),
        funded(123_456),
        Uint128::zero(),
        Uint128::zero(),
    )
    .unwrap();

    assert_eq!(result.entitlements.total_claim_ngonka, Uint128::zero());
    assert_eq!(
        result.entitlements.buyer_entitlement_ngonka,
        Uint128::zero()
    );
    assert_eq!(result.entitlements.host_entitlement_ngonka, Uint128::zero());
    assert_eq!(result.usdt.gross_micro_usdt, Uint128::zero());
    assert_eq!(result.usdt.fee_micro_usdt, Uint128::zero());
    assert_eq!(result.usdt.host_net_micro_usdt, Uint128::zero());
    assert_eq!(result.usdt.buyer_refund_micro_usdt, Uint128::new(123_456));
}

#[test]
fn one_ngonka_and_micro_usdt_dust_round_down_without_value_creation() {
    let result =
        calculate_claim_settlement(offer(1, 3), funded(1), Uint128::one(), Uint128::zero())
            .unwrap();

    assert_eq!(
        result.entitlements.effective_funded_capacity_ngonka,
        Uint128::new(333_333_333)
    );
    assert_eq!(result.entitlements.buyer_entitlement_ngonka, Uint128::one());
    assert_eq!(result.usdt.gross_micro_usdt, Uint128::zero());
    assert_eq!(result.usdt.fee_micro_usdt, Uint128::zero());
    assert_eq!(result.usdt.buyer_refund_micro_usdt, Uint128::one());
}

#[test]
fn gross_and_fee_use_floor_rounding() {
    let gross_dust = calculate_claim_settlement(
        offer(1, 3),
        funded(1),
        Uint128::new(333_333_333),
        Uint128::zero(),
    )
    .unwrap();
    assert_eq!(gross_dust.usdt.gross_micro_usdt, Uint128::zero());

    let fee_below_one =
        calculate_claim_settlement(offer(66, 1), funded(66), whole_gnk(66), Uint128::zero())
            .unwrap();
    assert_eq!(fee_below_one.usdt.gross_micro_usdt, Uint128::new(66));
    assert_eq!(fee_below_one.usdt.fee_micro_usdt, Uint128::zero());

    let fee_reaches_one =
        calculate_claim_settlement(offer(67, 1), funded(67), whole_gnk(67), Uint128::zero())
            .unwrap();
    assert_eq!(fee_reaches_one.usdt.gross_micro_usdt, Uint128::new(67));
    assert_eq!(fee_reaches_one.usdt.fee_micro_usdt, Uint128::one());
}

#[test]
fn funded_budget_mismatch_and_claim_overflow_are_typed_errors() {
    assert_eq!(
        calculate_claim_settlement(offer(10, 1), funded(9), Uint128::zero(), Uint128::zero(),)
            .unwrap_err(),
        MathError::FundedBudgetMismatch {
            configured_micro_usdt: Uint128::new(10),
            actual_micro_usdt: Uint128::new(9),
        }
    );
    assert_eq!(
        calculate_claim_settlement(offer(1, 1), funded(1), Uint128::MAX, Uint128::one(),)
            .unwrap_err(),
        MathError::ArithmeticOverflow {
            operation: "work claim + reward claim",
        }
    );
}

#[test]
fn maximum_representable_settlement_uses_wide_intermediates() {
    let result = calculate_claim_settlement(
        OfferTerms {
            configured_budget_micro_usdt: Uint128::MAX,
            price_micro_usdt_per_gnk: Uint128::new(GNK_SCALE),
        },
        BuyerFunding::Funded {
            actual_funded_budget_micro_usdt: Uint128::MAX,
        },
        Uint128::MAX,
        Uint128::zero(),
    )
    .unwrap();

    assert_eq!(result.entitlements.buyer_entitlement_ngonka, Uint128::MAX);
    assert_eq!(result.usdt.gross_micro_usdt, Uint128::MAX);
    assert_eq!(result.usdt.buyer_refund_micro_usdt, Uint128::zero());
    assert_eq!(
        result
            .usdt
            .host_net_micro_usdt
            .checked_add(result.usdt.fee_micro_usdt)
            .unwrap(),
        Uint128::MAX
    );
}

#[test]
fn permanent_80_20_share_distributes_all_balance_before_and_after_completion() {
    let first = released(
        calculate_cumulative_release(CumulativeReleaseInput {
            buyer_share_numerator: Uint128::new(80),
            share_denominator: Uint128::new(100),
            buyer_previously_released_ngonka: Uint256::zero(),
            host_previously_released_ngonka: Uint256::zero(),
            available_balance_ngonka: Uint128::new(25),
        })
        .unwrap(),
    );
    assert_eq!(first.buyer_delta_ngonka, Uint128::new(20));
    assert_eq!(first.host_delta_ngonka, Uint128::new(5));

    let second = released(
        calculate_cumulative_release(CumulativeReleaseInput {
            buyer_share_numerator: Uint128::new(80),
            share_denominator: Uint128::new(100),
            buyer_previously_released_ngonka: first.buyer_released_ngonka,
            host_previously_released_ngonka: first.host_released_ngonka,
            available_balance_ngonka: Uint128::new(125),
        })
        .unwrap(),
    );
    assert_eq!(second.released_total_ngonka, Uint256::from(150_u128));
    assert_eq!(second.buyer_released_ngonka, Uint256::from(120_u128));
    assert_eq!(second.host_released_ngonka, Uint256::from(30_u128));
    assert_eq!(second.buyer_delta_ngonka, Uint128::new(100));
    assert_eq!(second.host_delta_ngonka, Uint128::new(25));

    let late = released(
        calculate_cumulative_release(CumulativeReleaseInput {
            buyer_share_numerator: Uint128::new(80),
            share_denominator: Uint128::new(100),
            buyer_previously_released_ngonka: second.buyer_released_ngonka,
            host_previously_released_ngonka: second.host_released_ngonka,
            available_balance_ngonka: Uint128::new(10),
        })
        .unwrap(),
    );
    assert_eq!(late.buyer_released_ngonka, Uint256::from(128_u128));
    assert_eq!(late.host_released_ngonka, Uint256::from(32_u128));
    assert_eq!(late.buyer_delta_ngonka, Uint128::new(8));
    assert_eq!(late.host_delta_ngonka, Uint128::new(2));
}

#[test]
fn one_third_share_rounds_cumulatively_one_ngonka_at_a_time() {
    let mut buyer_paid = Uint256::zero();
    let mut host_paid = Uint256::zero();
    let expected = [(0, 1), (0, 2), (1, 2), (1, 3)];

    for (expected_buyer, expected_host) in expected {
        let amounts = released(
            calculate_cumulative_release(CumulativeReleaseInput {
                buyer_share_numerator: Uint128::one(),
                share_denominator: Uint128::new(3),
                buyer_previously_released_ngonka: buyer_paid,
                host_previously_released_ngonka: host_paid,
                available_balance_ngonka: Uint128::one(),
            })
            .unwrap(),
        );
        buyer_paid = amounts.buyer_released_ngonka;
        host_paid = amounts.host_released_ngonka;
        assert_eq!(buyer_paid, Uint256::from(expected_buyer as u128));
        assert_eq!(host_paid, Uint256::from(expected_host as u128));
        assert_eq!(
            amounts.buyer_delta_ngonka + amounts.host_delta_ngonka,
            Uint128::one()
        );
    }
}

#[test]
fn zero_and_full_buyer_shares_suppress_zero_side_deltas() {
    for (buyer_numerator, expected_buyer, expected_host) in [(0, 0, 17), (1, 17, 0)] {
        let amounts = released(
            calculate_cumulative_release(CumulativeReleaseInput {
                buyer_share_numerator: Uint128::new(buyer_numerator),
                share_denominator: Uint128::one(),
                buyer_previously_released_ngonka: Uint256::zero(),
                host_previously_released_ngonka: Uint256::zero(),
                available_balance_ngonka: Uint128::new(17),
            })
            .unwrap(),
        );
        assert_eq!(amounts.buyer_delta_ngonka, Uint128::new(expected_buyer));
        assert_eq!(amounts.host_delta_ngonka, Uint128::new(expected_host));
    }
}

#[test]
fn release_rejects_invalid_shares_and_noncanonical_lifetime_counters() {
    let base = CumulativeReleaseInput {
        buyer_share_numerator: Uint128::new(2),
        share_denominator: Uint128::new(3),
        buyer_previously_released_ngonka: Uint256::zero(),
        host_previously_released_ngonka: Uint256::zero(),
        available_balance_ngonka: Uint128::one(),
    };
    for (numerator, denominator) in [(0, 0), (4, 3)] {
        assert_eq!(
            calculate_cumulative_release(CumulativeReleaseInput {
                buyer_share_numerator: Uint128::new(numerator),
                share_denominator: Uint128::new(denominator),
                ..base
            })
            .unwrap_err(),
            MathError::InvalidReleaseShare {
                buyer_share_numerator: Uint128::new(numerator),
                share_denominator: Uint128::new(denominator),
            }
        );
    }
    assert!(matches!(
        calculate_cumulative_release(CumulativeReleaseInput {
            buyer_previously_released_ngonka: Uint256::one(),
            ..base
        }),
        Err(MathError::NonCanonicalReleaseCounters { .. })
    ));
}

#[test]
fn lifetime_counters_and_mul_div_use_wide_checked_arithmetic() {
    let previous = Uint256::from(Uint128::MAX) + Uint256::one();
    let amounts = released(
        calculate_cumulative_release(CumulativeReleaseInput {
            buyer_share_numerator: Uint128::one(),
            share_denominator: Uint128::one(),
            buyer_previously_released_ngonka: previous,
            host_previously_released_ngonka: Uint256::zero(),
            available_balance_ngonka: Uint128::MAX,
        })
        .unwrap(),
    );
    assert_eq!(
        amounts.released_total_ngonka,
        previous + Uint256::from(Uint128::MAX)
    );
    assert_eq!(amounts.buyer_delta_ngonka, Uint128::MAX);
    assert_eq!(amounts.host_delta_ngonka, Uint128::zero());

    assert_eq!(
        calculate_cumulative_release(CumulativeReleaseInput {
            buyer_share_numerator: Uint128::MAX,
            share_denominator: Uint128::MAX,
            buyer_previously_released_ngonka: Uint256::MAX,
            host_previously_released_ngonka: Uint256::zero(),
            available_balance_ngonka: Uint128::zero(),
        })
        .unwrap(),
        CumulativeReleaseResult::NoNewRelease
    );
    assert!(matches!(
        calculate_cumulative_release(CumulativeReleaseInput {
            buyer_share_numerator: Uint128::one(),
            share_denominator: Uint128::one(),
            buyer_previously_released_ngonka: Uint256::MAX,
            host_previously_released_ngonka: Uint256::zero(),
            available_balance_ngonka: Uint128::one(),
        }),
        Err(MathError::ArithmeticOverflow {
            operation: "previous released total + available balance"
        })
    ));
}

proptest! {
    #[test]
    fn funded_settlement_preserves_all_economic_invariants(
        work in any::<u64>(),
        reward in any::<u64>(),
        budget in any::<u64>(),
        price in 1_u64..=1_000_000_000_000_u64,
    ) {
        let result = calculate_claim_settlement(
            offer(u128::from(budget), u128::from(price)),
            funded(u128::from(budget)),
            Uint128::from(work),
            Uint128::from(reward),
        ).unwrap();

        let entitlements = result.entitlements;
        let usdt = result.usdt;
        prop_assert_eq!(
            entitlements.buyer_entitlement_ngonka.checked_add(entitlements.host_entitlement_ngonka).unwrap(),
            entitlements.total_claim_ngonka
        );
        prop_assert!(entitlements.buyer_entitlement_ngonka <= entitlements.effective_funded_capacity_ngonka);
        prop_assert!(usdt.gross_micro_usdt <= usdt.actual_funded_budget_micro_usdt);
        prop_assert_eq!(
            Uint256::from(usdt.host_net_micro_usdt)
                + Uint256::from(usdt.fee_micro_usdt)
                + Uint256::from(usdt.buyer_refund_micro_usdt),
            Uint256::from(usdt.actual_funded_budget_micro_usdt)
        );
    }

    #[test]
    fn permanent_release_is_partition_independent_monotonic_and_conserving(
        total in 1_u64..=1_000_000_u64,
        buyer_seed in any::<u64>(),
        tranches in prop::collection::vec(0_u16..=1_000_u16, 0..24),
    ) {
        let buyer_entitlement = buyer_seed % (total + 1);
        let mut sequence: Vec<u128> = tranches.into_iter().map(u128::from).collect();
        // Every generated case crosses the original claim, proving the same
        // properties for lifetime turnover above T as well as below it.
        sequence.push(u128::from(total) * 2 + 1);
        let run = |sequence: &[u128]| {
            let mut buyer_released = Uint256::zero();
            let mut host_released = Uint256::zero();
            let total = Uint128::from(total);
            let buyer_entitlement = Uint128::from(buyer_entitlement);

            for tranche in sequence {
                let available = Uint128::from(*tranche);
                match calculate_cumulative_release(CumulativeReleaseInput {
                    buyer_share_numerator: buyer_entitlement,
                    share_denominator: total,
                    buyer_previously_released_ngonka: buyer_released,
                    host_previously_released_ngonka: host_released,
                    available_balance_ngonka: available,
                }).unwrap() {
                    CumulativeReleaseResult::NoNewRelease => prop_assert!(available.is_zero()),
                    CumulativeReleaseResult::Release(amounts) => {
                        prop_assert!(amounts.buyer_released_ngonka >= buyer_released);
                        prop_assert!(amounts.host_released_ngonka >= host_released);
                        prop_assert_eq!(
                            amounts.buyer_released_ngonka + amounts.host_released_ngonka,
                            amounts.released_total_ngonka
                        );
                        prop_assert_eq!(amounts.buyer_delta_ngonka + amounts.host_delta_ngonka, available);
                        buyer_released = amounts.buyer_released_ngonka;
                        host_released = amounts.host_released_ngonka;
                    }
                }
            }
            prop_assert_eq!(
                calculate_cumulative_release(CumulativeReleaseInput {
                    buyer_share_numerator: buyer_entitlement,
                    share_denominator: total,
                    buyer_previously_released_ngonka: buyer_released,
                    host_previously_released_ngonka: host_released,
                    available_balance_ngonka: Uint128::zero(),
                }).unwrap(),
                CumulativeReleaseResult::NoNewRelease
            );
            Ok((buyer_released, host_released))
        };

        let forward = run(&sequence)?;
        let mut reversed = sequence.clone();
        reversed.reverse();
        let backward = run(&reversed)?;
        let aggregate = sequence.iter().copied().sum::<u128>();
        let direct = run(&[aggregate])?;
        prop_assert_eq!(forward, backward);
        prop_assert_eq!(forward, direct);
        prop_assert_eq!(forward.0 + forward.1, Uint256::from(aggregate));
        prop_assert!(forward.0 + forward.1 > Uint256::from(total));
    }
}
