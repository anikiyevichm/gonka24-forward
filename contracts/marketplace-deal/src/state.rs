use cosmwasm_schema::cw_serde;
use cosmwasm_std::{Addr, Uint128, Uint256};
use cw_storage_plus::Item;
use marketplace_api::deal::{DealStatus, GnkReleasePolicy, RefundReason};

#[cw_serde]
pub struct DealConfig {
    pub factory: Addr,
    pub host: Addr,
    pub target_epoch: u64,
    pub price_micro_usdt_per_gnk: Uint128,
    pub buyer_budget_micro_usdt: Uint128,
    pub funded_capacity_ngonka: Uint128,
    pub settlement_cw20: Addr,
    pub fee_recipient: Addr,
    pub fee_bps: u16,
    pub pinned_gonka_sha: String,
}

#[cw_serde]
pub struct DealState {
    pub status: DealStatus,
    pub refund_reason: Option<RefundReason>,
    pub buyer: Option<Addr>,
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

impl DealState {
    pub fn open() -> Self {
        Self {
            status: DealStatus::Open,
            refund_reason: None,
            buyer: None,
            recipient_locked: false,
            work_ngonka: Uint128::zero(),
            reward_ngonka: Uint128::zero(),
            total_claim_ngonka: Uint128::zero(),
            buyer_entitlement_ngonka: Uint128::zero(),
            host_entitlement_ngonka: Uint128::zero(),
            gnk_release_policy: GnkReleasePolicy::Unset,
            released_total_ngonka: Uint256::zero(),
            buyer_released_ngonka: Uint256::zero(),
            host_released_ngonka: Uint256::zero(),
            gross_usdt: Uint128::zero(),
            fee_usdt: Uint128::zero(),
            host_net_usdt: Uint128::zero(),
            buyer_refund_usdt: Uint128::zero(),
        }
    }
}

pub const CONFIG: Item<DealConfig> = Item::new("config");
pub const STATE: Item<DealState> = Item::new("state");

/// Outstanding settlement obligations, independent of the GNK lifecycle.
#[cw_serde]
#[derive(Default)]
pub struct PendingUsdt {
    pub host: Uint128,
    pub fee: Uint128,
    pub buyer: Uint128,
}

pub const PENDING_USDT: Item<PendingUsdt> = Item::new("pending_usdt");
