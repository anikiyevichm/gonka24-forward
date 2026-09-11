use cosmwasm_schema::{cw_serde, QueryResponses};
use cosmwasm_std::Uint128;

/// Immutable configuration supplied when the Factory is deployed.
#[cw_serde]
pub struct InstantiateMsg {
    pub deal_code_id: u64,
    pub settlement_cw20: String,
    pub fee_recipient: String,
    pub fee_bps: u16,
}

/// Public state-changing Factory operations.
#[cw_serde]
pub enum ExecuteMsg {
    CreateOffer {
        target_epoch: u64,
        price_micro_usdt_per_gnk: Uint128,
        buyer_budget_micro_usdt: Uint128,
    },
}

#[cw_serde]
#[derive(QueryResponses)]
pub enum QueryMsg {
    #[returns(ConfigResponse)]
    Config {},
    #[returns(DealResponse)]
    Deal { id: u64 },
    #[returns(DealResponse)]
    DealByHostEpoch { host: String, epoch: u64 },
    #[returns(ListDealsResponse)]
    ListDeals {
        start_after: Option<u64>,
        limit: Option<u32>,
    },
}

#[cw_serde]
pub struct ConfigResponse {
    pub deal_code_id: u64,
    pub settlement_cw20: String,
    pub fee_recipient: String,
    pub fee_bps: u16,
}

#[cw_serde]
pub struct DealResponse {
    pub address: String,
}

#[cw_serde]
pub struct DealListItem {
    pub id: u64,
    pub address: String,
}

#[cw_serde]
pub struct ListDealsResponse {
    pub deals: Vec<DealListItem>,
}
