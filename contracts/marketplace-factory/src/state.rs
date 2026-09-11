use cosmwasm_schema::cw_serde;
use cosmwasm_std::{Addr, Uint128};
use cw_storage_plus::{Item, Map};

#[cw_serde]
pub struct FactoryConfig {
    pub deal_code_id: u64,
    pub settlement_cw20: Addr,
    pub fee_recipient: Addr,
    pub fee_bps: u16,
}

/// Minimal context retained only between CreateOffer and its instantiate reply.
#[cw_serde]
pub struct PendingOffer {
    pub deal_id: u64,
    pub host: Addr,
    pub target_epoch: u64,
    pub price_micro_usdt_per_gnk: Uint128,
    pub buyer_budget_micro_usdt: Uint128,
}

pub const CONFIG: Item<FactoryConfig> = Item::new("config");
pub const NEXT_DEAL_ID: Item<u64> = Item::new("next_deal_id");
pub const PENDING_OFFER: Item<PendingOffer> = Item::new("pending_offer");
pub const DEALS: Map<u64, Addr> = Map::new("deals");
pub const DEAL_BY_HOST_EPOCH: Map<(Addr, u64), Addr> = Map::new("deal_by_host_epoch");
