use cosmwasm_std::{DepsMut, Env, Event, Reply, Response, SubMsgResult};
use cw_utils::parse_instantiate_response_data;

use crate::error::ContractError;
use crate::state::{DEALS, DEAL_BY_HOST_EPOCH, NEXT_DEAL_ID, PENDING_OFFER};

pub(crate) const INSTANTIATE_DEAL_REPLY_ID: u64 = 1;
pub(crate) const INSTANTIATE_RESPONSE_TYPE_URL: &str =
    "/cosmwasm.wasm.v1.MsgInstantiateContractResponse";

pub(crate) fn handle_reply(
    deps: DepsMut,
    _env: Env,
    msg: Reply,
) -> Result<Response, ContractError> {
    if msg.id != INSTANTIATE_DEAL_REPLY_ID {
        return Err(ContractError::UnexpectedReplyId { id: msg.id });
    }

    let pending = PENDING_OFFER
        .may_load(deps.storage)?
        .ok_or(ContractError::MissingPendingOffer)?;
    let response = match msg.result {
        SubMsgResult::Ok(response) => response,
        SubMsgResult::Err(_) => return Err(ContractError::DealInstantiationFailed),
    };
    if response.msg_responses.len() != 1 {
        return Err(ContractError::UnexpectedInstantiateResponseCount {
            actual: response.msg_responses.len(),
        });
    }
    let msg_response = &response.msg_responses[0];
    if msg_response.type_url != INSTANTIATE_RESPONSE_TYPE_URL {
        return Err(ContractError::UnexpectedInstantiateResponseType {
            actual: msg_response.type_url.clone(),
        });
    }
    let instantiate_response = parse_instantiate_response_data(&msg_response.value)
        .map_err(|_| ContractError::InvalidInstantiateResponse)?;
    let deal = deps
        .api
        .addr_validate(&instantiate_response.contract_address)
        .map_err(|_| ContractError::InvalidDealAddress)?;

    let next = NEXT_DEAL_ID.load(deps.storage)?;
    if next != pending.deal_id {
        return Err(ContractError::PendingDealIdMismatch {
            pending: pending.deal_id,
            next,
        });
    }
    let following = next.checked_add(1).ok_or(ContractError::DealIdOverflow)?;
    if DEALS.has(deps.storage, pending.deal_id) {
        return Err(ContractError::DealIdAlreadyRegistered {
            id: pending.deal_id,
        });
    }
    if DEAL_BY_HOST_EPOCH.has(deps.storage, (pending.host.clone(), pending.target_epoch)) {
        return Err(ContractError::DuplicateOffer {
            host: pending.host.to_string(),
            epoch: pending.target_epoch,
        });
    }

    DEALS.save(deps.storage, pending.deal_id, &deal)?;
    DEAL_BY_HOST_EPOCH.save(
        deps.storage,
        (pending.host.clone(), pending.target_epoch),
        &deal,
    )?;
    NEXT_DEAL_ID.save(deps.storage, &following)?;
    PENDING_OFFER.remove(deps.storage);

    Ok(Response::new().add_event(
        Event::new("offer_created")
            .add_attribute("deal_id", pending.deal_id.to_string())
            .add_attribute("deal", deal)
            .add_attribute("host", pending.host)
            .add_attribute("target_epoch", pending.target_epoch.to_string())
            .add_attribute("price_micro_usdt_per_gnk", pending.price_micro_usdt_per_gnk)
            .add_attribute("buyer_budget_micro_usdt", pending.buyer_budget_micro_usdt),
    ))
}
