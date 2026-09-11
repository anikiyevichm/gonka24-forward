use cosmwasm_schema::{cw_serde, QueryResponses};
use cosmwasm_std::{Uint128, Uint256};
use cw20::Cw20ReceiveMsg;

/// Immutable terms for one isolated Host/epoch deal.
#[cw_serde]
pub struct InstantiateMsg {
    pub factory: String,
    pub host: String,
    pub target_epoch: u64,
    pub price_micro_usdt_per_gnk: Uint128,
    pub buyer_budget_micro_usdt: Uint128,
    pub settlement_cw20: String,
    pub fee_recipient: String,
    pub fee_bps: u16,
    pub pinned_gonka_sha: String,
}

#[cw_serde]
pub enum ExecuteMsg {
    Receive(Cw20ReceiveMsg),
    Lock {},
    SettleClaim {},
    /// Permissionless; transfers only to the immutable recipient for this role.
    WithdrawUsdt {
        role: UsdtRole,
    },
    ReleaseUnlockedGnk {},
    Refund {},
    Cancel {},
    ForwardExcessGnk {},
}

#[cw_serde]
pub enum Cw20HookMsg {
    Fund {},
}

#[cw_serde]
#[derive(QueryResponses)]
pub enum QueryMsg {
    #[returns(ConfigResponse)]
    Config {},
    #[returns(StateResponse)]
    State {},
    #[returns(FundingResponse)]
    Funding {},
    #[returns(EntitlementsResponse)]
    Entitlements {},
    #[returns(ReleaseStatusResponse)]
    ReleaseStatus {},
    #[returns(NativeStatusResponse)]
    NativeStatus {},
    #[returns(UsdtPaymentsResponse)]
    UsdtPayments {},
}

#[cw_serde]
pub enum DealStatus {
    Open,
    Funded,
    Locked,
    Releasing,
    Completed,
    Refunded,
    Cancelled,
    Expired,
}

/// Frozen lifetime GNK distribution policy.
///
/// Positive claims use the exact `buyer_share_numerator / share_denominator`
/// fraction. Proven zero/no-claim outcomes use the explicit Host-only policy;
/// `Unset` means no outcome has established a right to distribute GNK yet.
#[cw_serde]
pub enum GnkReleasePolicy {
    Unset,
    Proportional {
        buyer_share_numerator: Uint128,
        share_denominator: Uint128,
    },
    HostOnly,
}

/// Authoritative reason for a completed permissionless `Refund {}` transition.
#[cw_serde]
pub enum RefundReason {
    RoutingMissing,
    RoutingMismatch,
    ClaimExpiry,
    NetworkUnconfirmed,
}

#[cw_serde]
pub struct ConfigResponse {
    pub factory: String,
    pub host: String,
    pub deal_address: String,
    pub target_epoch: u64,
    pub price_micro_usdt_per_gnk: Uint128,
    pub buyer_budget_micro_usdt: Uint128,
    pub funded_capacity_ngonka: Uint128,
    pub settlement_cw20: String,
    pub fee_recipient: String,
    pub fee_bps: u16,
    pub pinned_gonka_sha: String,
}

#[cw_serde]
pub struct StateResponse {
    pub status: DealStatus,
    pub refund_reason: Option<RefundReason>,
    pub buyer: Option<String>,
    pub recipient_locked: bool,
    pub work_ngonka: Uint128,
    pub reward_ngonka: Uint128,
    pub total_claim_ngonka: Uint128,
    pub buyer_entitlement_ngonka: Uint128,
    pub host_entitlement_ngonka: Uint128,
    pub gnk_release_policy: GnkReleasePolicy,
    pub released_total_ngonka: Uint256,
    pub buyer_released_ngonka: Uint256,
    pub host_released_ngonka: Uint256,
    pub gross_usdt: Uint128,
    pub fee_usdt: Uint128,
    pub host_net_usdt: Uint128,
    pub buyer_refund_usdt: Uint128,
}

#[cw_serde]
pub struct FundingResponse {
    pub buyer: Option<String>,
    pub buyer_budget_micro_usdt: Uint128,
    pub funded_capacity_ngonka: Uint128,
    pub funded: bool,
}

#[cw_serde]
pub struct EntitlementsResponse {
    pub work_ngonka: Uint128,
    pub reward_ngonka: Uint128,
    pub total_claim_ngonka: Uint128,
    pub buyer_entitlement_ngonka: Uint128,
    pub host_entitlement_ngonka: Uint128,
    pub gnk_release_policy: GnkReleasePolicy,
}

#[cw_serde]
pub struct ReleaseStatusResponse {
    /// Lifetime paid total; it is intentionally not capped by the claim.
    pub released_total_ngonka: Uint256,
    pub buyer_released_ngonka: Uint256,
    pub host_released_ngonka: Uint256,
    /// Unpaid part of the original settlement obligation, clamped at zero.
    /// These fields are diagnostics, not limits on future lifetime payouts.
    pub buyer_original_remaining_ngonka: Uint128,
    pub host_original_remaining_ngonka: Uint128,
    pub gnk_release_policy: GnkReleasePolicy,
}

#[cw_serde]
pub struct NativeStatusResponse {
    pub remaining_vesting_ngonka: Uint128,
    pub liquid_balance_ngonka: Uint128,
}

/// Accounting stays separate even when recipient addresses coincide.
#[cw_serde]
pub enum UsdtRole {
    Host,
    Fee,
    Buyer,
}

#[cw_serde]
pub struct UsdtPaymentResponse {
    pub recipient: Option<String>,
    pub accrued_micro_usdt: Uint128,
    pub paid_micro_usdt: Uint128,
    pub pending_micro_usdt: Uint128,
}

#[cw_serde]
pub struct UsdtPaymentsResponse {
    pub host: UsdtPaymentResponse,
    pub fee: UsdtPaymentResponse,
    pub buyer: UsdtPaymentResponse,
}
