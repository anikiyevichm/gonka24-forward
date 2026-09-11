use cosmwasm_std::{Uint128, Uint256};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MathError {
    #[error("price must be greater than zero")]
    ZeroPrice,

    #[error("arithmetic overflow while calculating {operation}")]
    ArithmeticOverflow { operation: &'static str },

    #[error(
        "actual funded budget {actual_micro_usdt} microUSDT does not match configured budget {configured_micro_usdt} microUSDT"
    )]
    FundedBudgetMismatch {
        configured_micro_usdt: Uint128,
        actual_micro_usdt: Uint128,
    },

    #[error(
        "buyer entitlement {buyer_entitlement_ngonka} ngonka exceeds total claim {total_claim_ngonka} ngonka"
    )]
    BuyerEntitlementExceedsTotalClaim {
        total_claim_ngonka: Uint128,
        buyer_entitlement_ngonka: Uint128,
    },

    #[error(
        "invalid GNK release share: buyer numerator {buyer_share_numerator}, denominator {share_denominator}"
    )]
    InvalidReleaseShare {
        buyer_share_numerator: Uint128,
        share_denominator: Uint128,
    },

    #[error(
        "release counters do not match the cumulative split: expected buyer {expected_buyer_released_ngonka} ngonka, got {actual_buyer_released_ngonka} ngonka"
    )]
    NonCanonicalReleaseCounters {
        expected_buyer_released_ngonka: Uint256,
        actual_buyer_released_ngonka: Uint256,
    },

    #[error(
        "GNK release does not conserve the available balance: available {available_balance_ngonka}, deltas {released_delta_ngonka}"
    )]
    ReleaseConservationMismatch {
        available_balance_ngonka: Uint128,
        released_delta_ngonka: Uint256,
    },

    #[error(
        "gross settlement {gross_micro_usdt} microUSDT exceeds actual funded budget {actual_funded_budget_micro_usdt} microUSDT"
    )]
    GrossExceedsFundedBudget {
        gross_micro_usdt: Uint128,
        actual_funded_budget_micro_usdt: Uint128,
    },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum GonkaQueryError {
    #[error("failed to encode protobuf request for {route}")]
    EncodeFailed { route: &'static str },

    #[error("Gonka query failed for {route}")]
    QueryFailed { route: &'static str },

    #[error("Gonka response for {route} is too large: {actual} bytes, maximum {maximum}")]
    ResponseTooLarge {
        route: &'static str,
        actual: usize,
        maximum: usize,
    },

    #[error("failed to decode protobuf response for {route}")]
    DecodeFailed { route: &'static str },
}
