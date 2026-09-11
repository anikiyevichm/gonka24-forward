use cosmwasm_std::{StdError, Uint128, Uint256};
use cw_utils::PaymentError;
use marketplace_api::deal::DealStatus;
use marketplace_common::error::{GonkaQueryError as CommonGonkaQueryError, MathError};
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum GonkaQueryError {
    #[error("failed to encode protobuf request for {route}")]
    EncodeFailed { route: &'static str },

    #[error("Gonka query failed for {route}")]
    QueryFailed { route: &'static str },

    #[error("Gonka does not support the required query route {route}")]
    UnsupportedRequest { route: &'static str },

    #[error("Gonka query infrastructure returned an invalid response for {route}")]
    InvalidResponse { route: &'static str },

    #[error("Gonka query infrastructure failed unexpectedly for {route}")]
    UnexpectedSystemError { route: &'static str },

    #[error("Gonka response for {route} is too large: {actual} bytes, maximum {maximum}")]
    ResponseTooLarge {
        route: &'static str,
        actual: usize,
        maximum: usize,
    },

    #[error("failed to decode protobuf response for {route}")]
    DecodeFailed { route: &'static str },

    #[error("Gonka response for {route} is missing required field {field}")]
    MissingField {
        route: &'static str,
        field: &'static str,
    },

    #[error("Gonka response for {route} has mismatched {field}")]
    IdentityMismatch {
        route: &'static str,
        field: &'static str,
    },

    #[error("Gonka response for {route} contains an invalid {field} address")]
    InvalidAddress {
        route: &'static str,
        field: &'static str,
    },

    #[error("claim recipient is not configured for epoch {epoch}")]
    ClaimRecipientNotFound { epoch: u64 },

    #[error("claim recipient is configured more than once for epoch {epoch}")]
    DuplicateClaimRecipient { epoch: u64 },

    #[error("claim recipient response contains {actual} entries, maximum {maximum}")]
    TooManyClaimRecipients { actual: usize, maximum: usize },

    #[error("Gonka response for {route} contains unexpected denom")]
    UnexpectedDenom { route: &'static str },

    #[error("Gonka response for {route} contains duplicate denom")]
    DuplicateDenom { route: &'static str },

    #[error("Gonka response for {route} contains an invalid amount")]
    InvalidAmount { route: &'static str },

    #[error("arithmetic overflow while calculating {operation}")]
    ArithmeticOverflow { operation: &'static str },
}

#[derive(Debug, Error, PartialEq)]
pub enum ContractError {
    #[error("no USDT remains payable for this role")]
    NothingToWithdraw,

    #[error("{0}")]
    Std(#[from] StdError),

    #[error("{0}")]
    Payment(#[from] PaymentError),

    #[error("{0}")]
    Gonka(#[from] GonkaQueryError),

    #[error("{0}")]
    CommonGonka(#[from] CommonGonkaQueryError),

    #[error("{0}")]
    Math(#[from] MathError),

    #[error("only the configured factory may instantiate this Deal")]
    UnauthorizedFactory,

    #[error("fee must be exactly {expected} bps, got {actual}")]
    InvalidFeeBps { expected: u16, actual: u16 },

    #[error("settlement token does not implement the required CW20 TokenInfo query")]
    InvalidSettlementToken,

    #[error("settlement token must use {expected} decimals, got {actual}")]
    InvalidSettlementDecimals { expected: u8, actual: u8 },

    #[error("price must be greater than zero")]
    ZeroPrice,

    #[error("buyer budget must be greater than zero")]
    ZeroBudget,

    #[error("target epoch {target} must be later than current epoch {current}")]
    InvalidTargetEpoch { current: u64, target: u64 },

    #[error("target epoch {target} exceeds maximum allowed epoch {maximum}")]
    EpochLookaheadExceeded { target: u64, maximum: u64 },

    #[error("current epoch cannot be safely extended by the maximum lookahead")]
    EpochLookaheadOverflow,

    #[error("buyer budget and price produce zero GNK capacity")]
    ZeroCapacity,

    #[error("pinned Gonka source SHA does not match the compiled contract")]
    PinnedGonkaShaMismatch,

    #[error("CW20 receive must be sent by the configured settlement token")]
    WrongCw20,

    #[error("cannot fund Deal in state {actual:?}; expected Open")]
    InvalidFundingState { actual: DealStatus },

    #[error("funding amount must be exactly {expected}, got {actual}")]
    WrongFundingAmount { expected: Uint128, actual: Uint128 },

    #[error("funding is closed at target epoch {target}; current epoch is {current}")]
    LateFunding { current: u64, target: u64 },

    #[error("cannot lock Deal in state {actual:?}; expected Open or Funded")]
    InvalidLockState { actual: DealStatus },

    #[error("lock window has not opened: current epoch {current}, target epoch {target}")]
    LockWindowNotOpen { current: u64, target: u64 },

    #[error(
        "lock window is closed: current epoch {current}, target epoch {target}, exclusive end {end_exclusive}"
    )]
    LockWindowClosed {
        current: u64,
        target: u64,
        end_exclusive: u64,
    },

    #[error("target epoch {target} cannot be safely extended by the routing proof window")]
    RoutingWindowOverflow { target: u64 },

    #[error("cannot cancel Deal in state {actual:?}; expected Open")]
    InvalidCancelState { actual: DealStatus },

    #[error("cannot cancel a Deal after a Buyer has funded it")]
    CannotCancelFundedDeal,

    #[error("only the immutable Host may cancel before target epoch {target}")]
    CancelBeforeTargetRequiresHost { target: u64 },

    #[error(
        "permissionless cancel window is closed: current epoch {current}, target epoch {target}, exclusive end {end_exclusive}"
    )]
    CancelWindowClosed {
        current: u64,
        target: u64,
        end_exclusive: u64,
    },

    #[error("claim recipient for epoch {epoch} still routes exactly to this Deal")]
    ClaimRecipientStillRouted { epoch: u64 },

    #[error(
        "cannot refund Deal in state {actual:?}; expected Funded for routing failure or Locked for claim expiry"
    )]
    InvalidRefundState { actual: DealStatus },

    #[error("refund window has not opened: current epoch {current}, target epoch {target}")]
    RefundWindowNotOpen { current: u64, target: u64 },

    #[error(
        "refund routing-proof window is closed: current epoch {current}, target epoch {target}, exclusive end {end_exclusive}"
    )]
    RefundWindowClosed {
        current: u64,
        target: u64,
        end_exclusive: u64,
    },

    #[error("claim recipient for epoch {epoch} still routes exactly to this Deal")]
    RefundRecipientStillRouted { epoch: u64 },

    #[error("Funded Deal is missing its immutable Buyer")]
    RefundWithoutBuyer,

    #[error("Funded Deal contains non-zero or already-finalized accounting before refund")]
    InconsistentRefundAccounting,

    #[error("target epoch {target} cannot be safely extended to a refund deadline")]
    ClaimExpiryEpochOverflow { target: u64 },

    #[error(
        "claim-expiry window has not opened: current epoch {current}, target epoch {target}, first allowed epoch {first_allowed}"
    )]
    ClaimExpiryWindowNotOpen {
        current: u64,
        target: u64,
        first_allowed: u64,
    },

    #[error("native claim for target epoch {epoch} is already confirmed")]
    ClaimAlreadyConfirmed { epoch: u64 },

    #[error("cannot settle claim in state {actual:?}; expected Locked")]
    InvalidSettlementState { actual: DealStatus },

    #[error("cannot settle a Locked Deal without the saved recipient lock proof")]
    MissingRecipientLockProof,

    #[error("native claim for target epoch {epoch} is not confirmed")]
    ClaimNotConfirmed { epoch: u64 },

    #[error("settlement calculated a Buyer refund but the Deal has no Buyer")]
    SettlementRefundWithoutBuyer,

    #[error(
        "cannot release GNK in state {actual:?}; expected Releasing, Completed, Refunded or Expired"
    )]
    InvalidReleaseState { actual: DealStatus },

    #[error(
        "stored claim accounting is inconsistent: work {work_ngonka} + reward {reward_ngonka} must equal total {total_claim_ngonka} ngonka"
    )]
    InconsistentClaimAccounting {
        work_ngonka: Uint128,
        reward_ngonka: Uint128,
        total_claim_ngonka: Uint128,
    },

    #[error(
        "stored entitlement accounting is inconsistent: buyer {buyer_entitlement_ngonka} + host {host_entitlement_ngonka} must equal total {total_claim_ngonka} ngonka"
    )]
    InconsistentEntitlementAccounting {
        buyer_entitlement_ngonka: Uint128,
        host_entitlement_ngonka: Uint128,
        total_claim_ngonka: Uint128,
    },

    #[error(
        "stored release accounting is inconsistent: buyer {buyer_released_ngonka} + host {host_released_ngonka} must equal released total {released_total_ngonka} ngonka"
    )]
    InconsistentReleaseAccounting {
        buyer_released_ngonka: Uint256,
        host_released_ngonka: Uint256,
        released_total_ngonka: Uint256,
    },

    #[error("stored GNK release policy is inconsistent with the frozen Deal outcome")]
    InconsistentReleasePolicy,

    #[error("release calculated a non-zero Buyer payment but the Deal has no Buyer")]
    BuyerReleaseWithoutBuyer,

    #[error("no additional GNK is currently available for release")]
    NothingToRelease,

    #[error("cannot forward excess GNK in state {actual:?}; expected Completed")]
    ExcessForwardingBeforeCompletion { actual: DealStatus },

    #[error("Completed Deal has no available excess GNK to forward")]
    NothingToForward,
}
