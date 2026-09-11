use cosmwasm_std::StdError;
use cw_utils::PaymentError;
use marketplace_common::error::{GonkaQueryError, MathError};
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum ContractError {
    #[error("{0}")]
    Std(#[from] StdError),

    #[error("{0}")]
    Payment(#[from] PaymentError),

    #[error("{0}")]
    Gonka(#[from] GonkaQueryError),

    #[error("{0}")]
    Math(#[from] MathError),

    #[error("deal code id must be greater than zero")]
    InvalidDealCodeId,

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

    #[error("an offer already exists for host {host} and epoch {epoch}")]
    DuplicateOffer { host: String, epoch: u64 },

    #[error("another offer instantiate operation is already pending")]
    PendingOfferExists,

    #[error("unexpected reply id: {id}")]
    UnexpectedReplyId { id: u64 },

    #[error("expected pending offer context is missing")]
    MissingPendingOffer,

    #[error("deal instantiate submessage returned an error")]
    DealInstantiationFailed,

    #[error("instantiate reply contained {actual} message responses; expected exactly one")]
    UnexpectedInstantiateResponseCount { actual: usize },

    #[error("instantiate reply had unexpected response type {actual}")]
    UnexpectedInstantiateResponseType { actual: String },

    #[error("instantiate reply response data is malformed")]
    InvalidInstantiateResponse,

    #[error("instantiate reply contains an invalid Deal address")]
    InvalidDealAddress,

    #[error("pending deal id {pending} does not match next deal id {next}")]
    PendingDealIdMismatch { pending: u64, next: u64 },

    #[error("deal id {id} is already registered")]
    DealIdAlreadyRegistered { id: u64 },

    #[error("next deal id overflow")]
    DealIdOverflow,
}
