#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    to_json_binary, Binary, Deps, DepsMut, Env, MessageInfo, Order, Reply, Response, StdResult,
    SubMsg, Uint128, WasmMsg,
};
use cw2::set_contract_version;
use cw20::{Cw20QueryMsg, TokenInfoResponse};
use cw_storage_plus::Bound;
use cw_utils::nonpayable;
use marketplace_api::deal::InstantiateMsg as DealInstantiateMsg;
use marketplace_api::factory::{
    ConfigResponse, DealListItem, DealResponse, ExecuteMsg, InstantiateMsg, ListDealsResponse,
    QueryMsg,
};
use marketplace_common::gonka::query_current_epoch;
use marketplace_common::math::funded_capacity_ngonka;

use crate::error::ContractError;
use crate::reply::INSTANTIATE_DEAL_REPLY_ID;
use crate::state::{
    FactoryConfig, PendingOffer, CONFIG, DEALS, DEAL_BY_HOST_EPOCH, NEXT_DEAL_ID, PENDING_OFFER,
};

const CONTRACT_NAME: &str = "crates.io:marketplace-factory";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROTOCOL_FEE_BPS: u16 = 150;
const SETTLEMENT_TOKEN_DECIMALS: u8 = 6;
const FIRST_DEAL_ID: u64 = 1;
const DEFAULT_LIST_LIMIT: u32 = 20;
const MAX_LIST_LIMIT: u32 = 50;
const MAX_EPOCH_LOOKAHEAD: u64 = 40;

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;

    if msg.deal_code_id == 0 {
        return Err(ContractError::InvalidDealCodeId);
    }
    if msg.fee_bps != PROTOCOL_FEE_BPS {
        return Err(ContractError::InvalidFeeBps {
            expected: PROTOCOL_FEE_BPS,
            actual: msg.fee_bps,
        });
    }

    let settlement_cw20 = deps.api.addr_validate(&msg.settlement_cw20)?;
    let fee_recipient = deps.api.addr_validate(&msg.fee_recipient)?;
    let token_info: TokenInfoResponse = deps
        .querier
        .query_wasm_smart(settlement_cw20.clone(), &Cw20QueryMsg::TokenInfo {})
        .map_err(|_| ContractError::InvalidSettlementToken)?;
    if token_info.decimals != SETTLEMENT_TOKEN_DECIMALS {
        return Err(ContractError::InvalidSettlementDecimals {
            expected: SETTLEMENT_TOKEN_DECIMALS,
            actual: token_info.decimals,
        });
    }

    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
    CONFIG.save(
        deps.storage,
        &FactoryConfig {
            deal_code_id: msg.deal_code_id,
            settlement_cw20: settlement_cw20.clone(),
            fee_recipient: fee_recipient.clone(),
            fee_bps: msg.fee_bps,
        },
    )?;
    NEXT_DEAL_ID.save(deps.storage, &FIRST_DEAL_ID)?;

    Ok(Response::new()
        .add_attribute("action", "instantiate")
        .add_attribute("deal_code_id", msg.deal_code_id.to_string())
        .add_attribute("settlement_cw20", settlement_cw20)
        .add_attribute("fee_recipient", fee_recipient)
        .add_attribute("fee_bps", msg.fee_bps.to_string()))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    match msg {
        ExecuteMsg::CreateOffer {
            target_epoch,
            price_micro_usdt_per_gnk,
            buyer_budget_micro_usdt,
        } => execute_create_offer(
            deps,
            env,
            info,
            target_epoch,
            price_micro_usdt_per_gnk,
            buyer_budget_micro_usdt,
        ),
    }
}

fn execute_create_offer(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    target_epoch: u64,
    price_micro_usdt_per_gnk: Uint128,
    buyer_budget_micro_usdt: Uint128,
) -> Result<Response, ContractError> {
    if PENDING_OFFER.may_load(deps.storage)?.is_some() {
        return Err(ContractError::PendingOfferExists);
    }
    if price_micro_usdt_per_gnk.is_zero() {
        return Err(ContractError::ZeroPrice);
    }
    if buyer_budget_micro_usdt.is_zero() {
        return Err(ContractError::ZeroBudget);
    }

    let host = info.sender;
    if DEAL_BY_HOST_EPOCH.has(deps.storage, (host.clone(), target_epoch)) {
        return Err(ContractError::DuplicateOffer {
            host: host.to_string(),
            epoch: target_epoch,
        });
    }

    let current_epoch = query_current_epoch(&deps.querier)?;
    if target_epoch <= current_epoch {
        return Err(ContractError::InvalidTargetEpoch {
            current: current_epoch,
            target: target_epoch,
        });
    }
    let maximum_epoch = current_epoch
        .checked_add(MAX_EPOCH_LOOKAHEAD)
        .ok_or(ContractError::EpochLookaheadOverflow)?;
    if target_epoch > maximum_epoch {
        return Err(ContractError::EpochLookaheadExceeded {
            target: target_epoch,
            maximum: maximum_epoch,
        });
    }

    let capacity = funded_capacity_ngonka(buyer_budget_micro_usdt, price_micro_usdt_per_gnk)?;
    if capacity.is_zero() {
        return Err(ContractError::ZeroCapacity);
    }

    let config = CONFIG.load(deps.storage)?;
    let deal_id = NEXT_DEAL_ID.load(deps.storage)?;
    PENDING_OFFER.save(
        deps.storage,
        &PendingOffer {
            deal_id,
            host: host.clone(),
            target_epoch,
            price_micro_usdt_per_gnk,
            buyer_budget_micro_usdt,
        },
    )?;

    let instantiate = WasmMsg::Instantiate {
        admin: None,
        code_id: config.deal_code_id,
        msg: to_json_binary(&DealInstantiateMsg {
            factory: env.contract.address.to_string(),
            host: host.to_string(),
            target_epoch,
            price_micro_usdt_per_gnk,
            buyer_budget_micro_usdt,
            settlement_cw20: config.settlement_cw20.to_string(),
            fee_recipient: config.fee_recipient.to_string(),
            fee_bps: config.fee_bps,
            pinned_gonka_sha: gonka_proto::SOURCE_COMMIT.to_string(),
        })?,
        funds: vec![],
        label: format!("gonka-forward-deal-{deal_id}"),
    };

    Ok(Response::new()
        .add_attribute("action", "create_offer")
        .add_submessage(SubMsg::reply_on_success(
            instantiate,
            INSTANTIATE_DEAL_REPLY_ID,
        )))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Config {} => to_json_binary(&query_config(deps)?),
        QueryMsg::Deal { id } => to_json_binary(&DealResponse {
            address: DEALS.load(deps.storage, id)?.to_string(),
        }),
        QueryMsg::DealByHostEpoch { host, epoch } => {
            let host = deps.api.addr_validate(&host)?;
            to_json_binary(&DealResponse {
                address: DEAL_BY_HOST_EPOCH
                    .load(deps.storage, (host, epoch))?
                    .to_string(),
            })
        }
        QueryMsg::ListDeals { start_after, limit } => {
            to_json_binary(&query_list_deals(deps, start_after, limit)?)
        }
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn reply(deps: DepsMut, env: Env, msg: Reply) -> Result<Response, ContractError> {
    crate::reply::handle_reply(deps, env, msg)
}

fn query_config(deps: Deps) -> StdResult<ConfigResponse> {
    let config = CONFIG.load(deps.storage)?;
    Ok(ConfigResponse {
        deal_code_id: config.deal_code_id,
        settlement_cw20: config.settlement_cw20.to_string(),
        fee_recipient: config.fee_recipient.to_string(),
        fee_bps: config.fee_bps,
    })
}

fn query_list_deals(
    deps: Deps,
    start_after: Option<u64>,
    limit: Option<u32>,
) -> StdResult<ListDealsResponse> {
    let limit = limit.unwrap_or(DEFAULT_LIST_LIMIT).min(MAX_LIST_LIMIT) as usize;
    let start = start_after.map(Bound::exclusive);
    let deals = DEALS
        .range(deps.storage, start, None, Order::Ascending)
        .take(limit)
        .map(|item| {
            let (id, address) = item?;
            Ok(DealListItem {
                id,
                address: address.to_string(),
            })
        })
        .collect::<StdResult<Vec<_>>>()?;
    Ok(ListDealsResponse { deals })
}

#[cfg(test)]
mod tests;
