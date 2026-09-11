use cosmwasm_std::{Uint128, Uint256, Uint512};

use crate::error::MathError;

pub const GNK_SCALE: u128 = 1_000_000_000;
pub const PROTOCOL_FEE_BPS: u128 = 150;
const BPS_DENOMINATOR: u128 = 10_000;

/// Immutable economic terms configured when an offer is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OfferTerms {
    pub configured_budget_micro_usdt: Uint128,
    pub price_micro_usdt_per_gnk: Uint128,
}

/// The effective funding state, kept distinct from immutable offer terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuyerFunding {
    NoBuyer,
    Funded {
        actual_funded_budget_micro_usdt: Uint128,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimEntitlements {
    pub work_ngonka: Uint128,
    pub reward_ngonka: Uint128,
    pub total_claim_ngonka: Uint128,
    pub effective_funded_capacity_ngonka: Uint128,
    pub buyer_entitlement_ngonka: Uint128,
    pub host_entitlement_ngonka: Uint128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsdtSettlement {
    pub actual_funded_budget_micro_usdt: Uint128,
    pub gross_micro_usdt: Uint128,
    pub fee_micro_usdt: Uint128,
    pub host_net_micro_usdt: Uint128,
    pub buyer_refund_micro_usdt: Uint128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimSettlement {
    pub entitlements: ClaimEntitlements,
    pub usdt: UsdtSettlement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CumulativeReleaseInput {
    pub buyer_share_numerator: Uint128,
    pub share_denominator: Uint128,
    pub buyer_previously_released_ngonka: Uint256,
    pub host_previously_released_ngonka: Uint256,
    pub available_balance_ngonka: Uint128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CumulativeReleaseAmounts {
    pub released_total_ngonka: Uint256,
    pub buyer_released_ngonka: Uint256,
    pub host_released_ngonka: Uint256,
    pub buyer_delta_ngonka: Uint128,
    pub host_delta_ngonka: Uint128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CumulativeReleaseResult {
    NoNewRelease,
    Release(CumulativeReleaseAmounts),
}

pub fn funded_capacity_ngonka(
    budget_micro_usdt: Uint128,
    price_micro_usdt_per_gnk: Uint128,
) -> Result<Uint128, MathError> {
    if price_micro_usdt_per_gnk.is_zero() {
        return Err(MathError::ZeroPrice);
    }

    let numerator = Uint256::from(budget_micro_usdt)
        .checked_mul(Uint256::from(GNK_SCALE))
        .map_err(|_| MathError::ArithmeticOverflow {
            operation: "buyer budget * GNK scale",
        })?;
    let capacity = numerator
        .checked_div(Uint256::from(price_micro_usdt_per_gnk))
        .map_err(|_| MathError::ArithmeticOverflow {
            operation: "scaled buyer budget / price",
        })?;

    Uint128::try_from(capacity).map_err(|_| MathError::ArithmeticOverflow {
        operation: "funded capacity conversion to Uint128",
    })
}

/// Calculates claim ownership and all USDT settlement amounts in one pass.
///
/// Capacity is derived from the actual deposit for funded deals. Requiring that
/// deposit to equal the immutable configured budget prevents a caller from
/// manufacturing a negative refund or an entitlement backed by another amount.
pub fn calculate_claim_settlement(
    offer: OfferTerms,
    buyer_funding: BuyerFunding,
    work_ngonka: Uint128,
    reward_ngonka: Uint128,
) -> Result<ClaimSettlement, MathError> {
    if offer.price_micro_usdt_per_gnk.is_zero() {
        return Err(MathError::ZeroPrice);
    }

    let total_claim_ngonka =
        work_ngonka
            .checked_add(reward_ngonka)
            .map_err(|_| MathError::ArithmeticOverflow {
                operation: "work claim + reward claim",
            })?;

    let (actual_funded_budget_micro_usdt, effective_funded_capacity_ngonka) = match buyer_funding {
        BuyerFunding::NoBuyer => (Uint128::zero(), Uint128::zero()),
        BuyerFunding::Funded {
            actual_funded_budget_micro_usdt,
        } => {
            if actual_funded_budget_micro_usdt != offer.configured_budget_micro_usdt {
                return Err(MathError::FundedBudgetMismatch {
                    configured_micro_usdt: offer.configured_budget_micro_usdt,
                    actual_micro_usdt: actual_funded_budget_micro_usdt,
                });
            }
            let capacity = funded_capacity_ngonka(
                actual_funded_budget_micro_usdt,
                offer.price_micro_usdt_per_gnk,
            )?;
            (actual_funded_budget_micro_usdt, capacity)
        }
    };

    let buyer_entitlement_ngonka = total_claim_ngonka.min(effective_funded_capacity_ngonka);
    let host_entitlement_ngonka = total_claim_ngonka
        .checked_sub(buyer_entitlement_ngonka)
        .map_err(|_| MathError::ArithmeticOverflow {
            operation: "total claim - buyer entitlement",
        })?;

    let gross_micro_usdt = mul_div_floor_to_uint128(
        buyer_entitlement_ngonka,
        offer.price_micro_usdt_per_gnk,
        Uint128::new(GNK_SCALE),
        "buyer entitlement * price",
        "gross settlement conversion to Uint128",
    )?;
    if gross_micro_usdt > actual_funded_budget_micro_usdt {
        return Err(MathError::GrossExceedsFundedBudget {
            gross_micro_usdt,
            actual_funded_budget_micro_usdt,
        });
    }

    let fee_micro_usdt = mul_div_floor_to_uint128(
        gross_micro_usdt,
        Uint128::new(PROTOCOL_FEE_BPS),
        Uint128::new(BPS_DENOMINATOR),
        "gross settlement * protocol fee bps",
        "protocol fee conversion to Uint128",
    )?;
    let host_net_micro_usdt = gross_micro_usdt.checked_sub(fee_micro_usdt).map_err(|_| {
        MathError::ArithmeticOverflow {
            operation: "gross settlement - protocol fee",
        }
    })?;
    let buyer_refund_micro_usdt = actual_funded_budget_micro_usdt
        .checked_sub(gross_micro_usdt)
        .map_err(|_| MathError::GrossExceedsFundedBudget {
            gross_micro_usdt,
            actual_funded_budget_micro_usdt,
        })?;

    Ok(ClaimSettlement {
        entitlements: ClaimEntitlements {
            work_ngonka,
            reward_ngonka,
            total_claim_ngonka,
            effective_funded_capacity_ngonka,
            buyer_entitlement_ngonka,
            host_entitlement_ngonka,
        },
        usdt: UsdtSettlement {
            actual_funded_budget_micro_usdt,
            gross_micro_usdt,
            fee_micro_usdt,
            host_net_micro_usdt,
            buyer_refund_micro_usdt,
        },
    })
}

/// Calculates the next cumulative GNK release without touching storage or funds.
///
/// Side counters are the source for the previous cumulative total. They must
/// match the same floor-based cumulative split used for every earlier release.
pub fn calculate_cumulative_release(
    input: CumulativeReleaseInput,
) -> Result<CumulativeReleaseResult, MathError> {
    if input.share_denominator.is_zero() || input.buyer_share_numerator > input.share_denominator {
        return Err(MathError::InvalidReleaseShare {
            buyer_share_numerator: input.buyer_share_numerator,
            share_denominator: input.share_denominator,
        });
    }

    let previously_released_ngonka = input
        .buyer_previously_released_ngonka
        .checked_add(input.host_previously_released_ngonka)
        .map_err(|_| MathError::ArithmeticOverflow {
            operation: "buyer released + host released",
        })?;
    let expected_buyer_released_ngonka = cumulative_buyer_target(
        previously_released_ngonka,
        input.buyer_share_numerator,
        input.share_denominator,
    )?;
    let expected_host_released_ngonka = previously_released_ngonka
        .checked_sub(expected_buyer_released_ngonka)
        .map_err(|_| MathError::ArithmeticOverflow {
            operation: "previous released total - expected buyer released",
        })?;
    if input.buyer_previously_released_ngonka != expected_buyer_released_ngonka
        || input.host_previously_released_ngonka != expected_host_released_ngonka
    {
        return Err(MathError::NonCanonicalReleaseCounters {
            expected_buyer_released_ngonka,
            actual_buyer_released_ngonka: input.buyer_previously_released_ngonka,
        });
    }

    if input.available_balance_ngonka.is_zero() {
        return Ok(CumulativeReleaseResult::NoNewRelease);
    }

    let released_total_ngonka = previously_released_ngonka
        .checked_add(Uint256::from(input.available_balance_ngonka))
        .map_err(|_| MathError::ArithmeticOverflow {
            operation: "previous released total + available balance",
        })?;

    let buyer_released_ngonka = cumulative_buyer_target(
        released_total_ngonka,
        input.buyer_share_numerator,
        input.share_denominator,
    )?;
    let host_released_ngonka = released_total_ngonka
        .checked_sub(buyer_released_ngonka)
        .map_err(|_| MathError::ArithmeticOverflow {
            operation: "released total - buyer released target",
        })?;
    let buyer_delta_wide = buyer_released_ngonka
        .checked_sub(input.buyer_previously_released_ngonka)
        .map_err(|_| MathError::ArithmeticOverflow {
            operation: "buyer released target - buyer previous release",
        })?;
    let host_delta_wide = host_released_ngonka
        .checked_sub(input.host_previously_released_ngonka)
        .map_err(|_| MathError::ArithmeticOverflow {
            operation: "host released target - host previous release",
        })?;
    let released_delta = buyer_delta_wide.checked_add(host_delta_wide).map_err(|_| {
        MathError::ArithmeticOverflow {
            operation: "buyer delta + host delta",
        }
    })?;
    if released_delta != Uint256::from(input.available_balance_ngonka) {
        return Err(MathError::ReleaseConservationMismatch {
            available_balance_ngonka: input.available_balance_ngonka,
            released_delta_ngonka: released_delta,
        });
    }
    let buyer_delta_ngonka =
        Uint128::try_from(buyer_delta_wide).map_err(|_| MathError::ArithmeticOverflow {
            operation: "buyer release delta conversion to Uint128",
        })?;
    let host_delta_ngonka =
        Uint128::try_from(host_delta_wide).map_err(|_| MathError::ArithmeticOverflow {
            operation: "host release delta conversion to Uint128",
        })?;

    Ok(CumulativeReleaseResult::Release(CumulativeReleaseAmounts {
        released_total_ngonka,
        buyer_released_ngonka,
        host_released_ngonka,
        buyer_delta_ngonka,
        host_delta_ngonka,
    }))
}

fn cumulative_buyer_target(
    released_total_ngonka: Uint256,
    buyer_share_numerator: Uint128,
    share_denominator: Uint128,
) -> Result<Uint256, MathError> {
    let numerator = Uint512::from(released_total_ngonka)
        .checked_mul(Uint512::from(buyer_share_numerator))
        .map_err(|_| MathError::ArithmeticOverflow {
            operation: "released total * buyer share numerator",
        })?;
    let quotient = numerator
        .checked_div(Uint512::from(share_denominator))
        .map_err(|_| MathError::ArithmeticOverflow {
            operation: "buyer cumulative share division",
        })?;
    Uint256::try_from(quotient).map_err(|_| MathError::ArithmeticOverflow {
        operation: "buyer cumulative release conversion to Uint256",
    })
}

fn mul_div_floor_to_uint128(
    left: Uint128,
    right: Uint128,
    denominator: Uint128,
    multiply_operation: &'static str,
    conversion_operation: &'static str,
) -> Result<Uint128, MathError> {
    let numerator = Uint256::from(left)
        .checked_mul(Uint256::from(right))
        .map_err(|_| MathError::ArithmeticOverflow {
            operation: multiply_operation,
        })?;
    let quotient = numerator
        .checked_div(Uint256::from(denominator))
        .map_err(|_| MathError::ArithmeticOverflow {
            operation: "floor division",
        })?;
    Uint128::try_from(quotient).map_err(|_| MathError::ArithmeticOverflow {
        operation: conversion_operation,
    })
}

#[cfg(test)]
mod tests;
