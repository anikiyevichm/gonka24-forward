#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    coin, from_json, to_json_binary, Addr, BankMsg, Binary, Deps, DepsMut, Env, Event, MessageInfo,
    Response, StdError, StdResult, Uint128, Uint256, WasmMsg,
};
use cw2::set_contract_version;
use cw20::{Cw20ExecuteMsg, Cw20QueryMsg, Cw20ReceiveMsg, TokenInfoResponse};
use cw_utils::nonpayable;
use marketplace_api::deal::{
    ConfigResponse, Cw20HookMsg, DealStatus, EntitlementsResponse, ExecuteMsg, FundingResponse,
    GnkReleasePolicy, InstantiateMsg, NativeStatusResponse, QueryMsg, RefundReason,
    ReleaseStatusResponse, StateResponse, UsdtPaymentResponse, UsdtPaymentsResponse, UsdtRole,
};
use marketplace_common::gonka::query_current_epoch;
use marketplace_common::math::{
    calculate_claim_settlement, calculate_cumulative_release, funded_capacity_ngonka, BuyerFunding,
    CumulativeReleaseInput, CumulativeReleaseResult, OfferTerms,
};

use crate::error::ContractError;
use crate::gonka::{
    query_claim_recipient, query_claim_recipient_routing, query_epoch_performance,
    query_ngonka_balance, query_total_vesting, ClaimRecipientRouting, NGONKA_DENOM,
};
use crate::state::{DealConfig, DealState, PendingUsdt, CONFIG, PENDING_USDT, STATE};

const CONTRACT_NAME: &str = "crates.io:marketplace-deal";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROTOCOL_FEE_BPS: u16 = 150;
const SETTLEMENT_TOKEN_DECIMALS: u8 = 6;
const MAX_EPOCH_LOOKAHEAD: u64 = 40;
const CLAIM_RECIPIENT_PRUNING_THRESHOLD: u64 = 5;
const CLAIM_EXPIRY_DELAY_EPOCHS: u64 = 2;
const NETWORK_UNCONFIRMED_DELAY_EPOCHS: u64 = 3;

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;

    let factory = deps.api.addr_validate(&msg.factory)?;
    if info.sender != factory {
        return Err(ContractError::UnauthorizedFactory);
    }
    let host = deps.api.addr_validate(&msg.host)?;
    let settlement_cw20 = deps.api.addr_validate(&msg.settlement_cw20)?;
    let fee_recipient = deps.api.addr_validate(&msg.fee_recipient)?;

    if msg.fee_bps != PROTOCOL_FEE_BPS {
        return Err(ContractError::InvalidFeeBps {
            expected: PROTOCOL_FEE_BPS,
            actual: msg.fee_bps,
        });
    }
    if msg.price_micro_usdt_per_gnk.is_zero() {
        return Err(ContractError::ZeroPrice);
    }
    if msg.buyer_budget_micro_usdt.is_zero() {
        return Err(ContractError::ZeroBudget);
    }
    if msg.pinned_gonka_sha != gonka_proto::SOURCE_COMMIT {
        return Err(ContractError::PinnedGonkaShaMismatch);
    }

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

    let current_epoch = query_current_epoch(&deps.querier)?;
    if msg.target_epoch <= current_epoch {
        return Err(ContractError::InvalidTargetEpoch {
            current: current_epoch,
            target: msg.target_epoch,
        });
    }
    let maximum_epoch = current_epoch
        .checked_add(MAX_EPOCH_LOOKAHEAD)
        .ok_or(ContractError::EpochLookaheadOverflow)?;
    if msg.target_epoch > maximum_epoch {
        return Err(ContractError::EpochLookaheadExceeded {
            target: msg.target_epoch,
            maximum: maximum_epoch,
        });
    }

    let funded_capacity_ngonka =
        funded_capacity_ngonka(msg.buyer_budget_micro_usdt, msg.price_micro_usdt_per_gnk)?;
    if funded_capacity_ngonka.is_zero() {
        return Err(ContractError::ZeroCapacity);
    }

    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
    CONFIG.save(
        deps.storage,
        &DealConfig {
            factory: factory.clone(),
            host: host.clone(),
            target_epoch: msg.target_epoch,
            price_micro_usdt_per_gnk: msg.price_micro_usdt_per_gnk,
            buyer_budget_micro_usdt: msg.buyer_budget_micro_usdt,
            funded_capacity_ngonka,
            settlement_cw20: settlement_cw20.clone(),
            fee_recipient: fee_recipient.clone(),
            fee_bps: msg.fee_bps,
            pinned_gonka_sha: msg.pinned_gonka_sha,
        },
    )?;
    STATE.save(deps.storage, &DealState::open())?;

    Ok(Response::new()
        .add_attribute("action", "instantiate")
        .add_attribute("deal", env.contract.address)
        .add_attribute("factory", factory)
        .add_attribute("host", host)
        .add_attribute("target_epoch", msg.target_epoch.to_string())
        .add_attribute("price_micro_usdt_per_gnk", msg.price_micro_usdt_per_gnk)
        .add_attribute("buyer_budget_micro_usdt", msg.buyer_budget_micro_usdt)
        .add_attribute("funded_capacity_ngonka", funded_capacity_ngonka)
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
        ExecuteMsg::Receive(receive) => execute_receive(deps, env, info, receive),
        ExecuteMsg::Lock {} => execute_lock(deps, env),
        ExecuteMsg::SettleClaim {} => execute_settle_claim(deps, env),
        ExecuteMsg::WithdrawUsdt { role } => execute_withdraw_usdt(deps, env, role),
        ExecuteMsg::ReleaseUnlockedGnk {} => execute_release_unlocked_gnk(deps, env),
        ExecuteMsg::Refund {} => execute_refund(deps, env),
        ExecuteMsg::Cancel {} => execute_cancel(deps, env, info),
        ExecuteMsg::ForwardExcessGnk {} => execute_forward_excess_gnk(deps, env),
    }
}

fn execute_release_unlocked_gnk(deps: DepsMut, env: Env) -> Result<Response, ContractError> {
    execute_gnk_release(deps, env, GnkReleaseEntryPoint::ReleaseUnlocked)
}

fn execute_forward_excess_gnk(deps: DepsMut, env: Env) -> Result<Response, ContractError> {
    execute_gnk_release(deps, env, GnkReleaseEntryPoint::ForwardExcessAlias)
}

#[derive(Clone, Copy)]
enum GnkReleaseEntryPoint {
    ReleaseUnlocked,
    ForwardExcessAlias,
}

fn execute_gnk_release(
    deps: DepsMut,
    env: Env,
    entry_point: GnkReleaseEntryPoint,
) -> Result<Response, ContractError> {
    let config = CONFIG.load(deps.storage)?;
    let mut state = STATE.load(deps.storage)?;
    match entry_point {
        GnkReleaseEntryPoint::ReleaseUnlocked
            if !matches!(
                state.status,
                DealStatus::Releasing
                    | DealStatus::Completed
                    | DealStatus::Refunded
                    | DealStatus::Expired
            ) =>
        {
            return Err(ContractError::InvalidReleaseState {
                actual: state.status,
            });
        }
        GnkReleaseEntryPoint::ForwardExcessAlias if state.status != DealStatus::Completed => {
            return Err(ContractError::ExcessForwardingBeforeCompletion {
                actual: state.status,
            });
        }
        _ => {}
    }
    let (buyer_share_numerator, share_denominator) = validate_frozen_gnk_accounting(&state)?;
    let host_share_numerator = share_denominator
        .checked_sub(buyer_share_numerator)
        .map_err(|_| ContractError::InconsistentReleasePolicy)?;

    // Pinned Gonka keeps locked streamvesting coins in the module account and
    // transfers only unlocked tranches to the Deal. Therefore this standard
    // bank balance is the amount presently available to BankMsg::Send in that
    // model. Real-chain verification remains a release gate.
    let available_balance_ngonka = query_ngonka_balance(&deps.querier, &env.contract.address)?;
    let release = calculate_cumulative_release(CumulativeReleaseInput {
        buyer_share_numerator,
        share_denominator,
        buyer_previously_released_ngonka: state.buyer_released_ngonka,
        host_previously_released_ngonka: state.host_released_ngonka,
        available_balance_ngonka,
    })?;
    let CumulativeReleaseResult::Release(amounts) = release else {
        return Err(match entry_point {
            GnkReleaseEntryPoint::ReleaseUnlocked => ContractError::NothingToRelease,
            GnkReleaseEntryPoint::ForwardExcessAlias => ContractError::NothingToForward,
        });
    };

    let buyer_recipient = if amounts.buyer_delta_ngonka.is_zero() {
        None
    } else {
        Some(
            state
                .buyer
                .clone()
                .ok_or(ContractError::BuyerReleaseWithoutBuyer)?,
        )
    };

    state.released_total_ngonka = amounts.released_total_ngonka;
    state.buyer_released_ngonka = amounts.buyer_released_ngonka;
    state.host_released_ngonka = amounts.host_released_ngonka;
    let completed_now = state.status == DealStatus::Releasing
        && amounts.released_total_ngonka >= Uint256::from(state.total_claim_ngonka);
    if completed_now {
        state.status = DealStatus::Completed;
    }

    // The write intentionally precedes all BankMsg values. Cosmos transaction
    // atomicity rolls it and every earlier transfer back if any send fails.
    STATE.save(deps.storage, &state)?;

    let mut released_event = Event::new("gnk_released")
        .add_attribute("deal", env.contract.address.clone())
        .add_attribute("host", config.host.clone())
        .add_attribute("target_epoch", config.target_epoch.to_string())
        .add_attribute("released_total_ngonka", amounts.released_total_ngonka)
        .add_attribute("buyer_released_ngonka", amounts.buyer_released_ngonka)
        .add_attribute("host_released_ngonka", amounts.host_released_ngonka)
        .add_attribute("buyer_delta_ngonka", amounts.buyer_delta_ngonka)
        .add_attribute("host_delta_ngonka", amounts.host_delta_ngonka)
        .add_attribute("buyer_share_numerator", buyer_share_numerator)
        .add_attribute("host_share_numerator", host_share_numerator)
        .add_attribute("share_denominator", share_denominator)
        .add_attribute(
            "entry_point",
            match entry_point {
                GnkReleaseEntryPoint::ReleaseUnlocked => "release_unlocked_gnk",
                GnkReleaseEntryPoint::ForwardExcessAlias => "forward_excess_gnk",
            },
        )
        .add_attribute("available_balance_ngonka", available_balance_ngonka);
    if let Some(buyer) = state.buyer.as_ref() {
        released_event = released_event.add_attribute("buyer", buyer);
    }
    let mut response = Response::new().add_event(released_event);

    if let Some(buyer) = buyer_recipient {
        response = response.add_message(BankMsg::Send {
            to_address: buyer.to_string(),
            amount: vec![coin(amounts.buyer_delta_ngonka.u128(), NGONKA_DENOM)],
        });
    }
    if !amounts.host_delta_ngonka.is_zero() {
        response = response.add_message(BankMsg::Send {
            to_address: config.host.to_string(),
            amount: vec![coin(amounts.host_delta_ngonka.u128(), NGONKA_DENOM)],
        });
    }
    if completed_now {
        response = response.add_event(deal_completed_event(
            &env,
            &config,
            state.total_claim_ngonka,
        ));
    }
    if matches!(entry_point, GnkReleaseEntryPoint::ForwardExcessAlias) {
        response = response.add_event(
            Event::new("excess_gnk_forwarded")
                .add_attribute("deal", env.contract.address)
                .add_attribute("host", config.host)
                .add_attribute("target_epoch", config.target_epoch.to_string())
                .add_attribute("amount_ngonka", available_balance_ngonka)
                .add_attribute("buyer_delta_ngonka", amounts.buyer_delta_ngonka)
                .add_attribute("host_delta_ngonka", amounts.host_delta_ngonka)
                .add_attribute("released_total_ngonka", amounts.released_total_ngonka),
        );
    }

    Ok(response)
}

fn validate_frozen_gnk_accounting(state: &DealState) -> Result<(Uint128, Uint128), ContractError> {
    if state.work_ngonka.checked_add(state.reward_ngonka).ok() != Some(state.total_claim_ngonka) {
        return Err(ContractError::InconsistentClaimAccounting {
            work_ngonka: state.work_ngonka,
            reward_ngonka: state.reward_ngonka,
            total_claim_ngonka: state.total_claim_ngonka,
        });
    }
    if state
        .buyer_entitlement_ngonka
        .checked_add(state.host_entitlement_ngonka)
        .ok()
        != Some(state.total_claim_ngonka)
    {
        return Err(ContractError::InconsistentEntitlementAccounting {
            buyer_entitlement_ngonka: state.buyer_entitlement_ngonka,
            host_entitlement_ngonka: state.host_entitlement_ngonka,
            total_claim_ngonka: state.total_claim_ngonka,
        });
    }
    if state
        .buyer_released_ngonka
        .checked_add(state.host_released_ngonka)
        .ok()
        != Some(state.released_total_ngonka)
    {
        return Err(ContractError::InconsistentReleaseAccounting {
            buyer_released_ngonka: state.buyer_released_ngonka,
            host_released_ngonka: state.host_released_ngonka,
            released_total_ngonka: state.released_total_ngonka,
        });
    }
    match &state.gnk_release_policy {
        GnkReleasePolicy::Proportional {
            buyer_share_numerator,
            share_denominator,
        } if !state.total_claim_ngonka.is_zero()
            && *buyer_share_numerator == state.buyer_entitlement_ngonka
            && *share_denominator == state.total_claim_ngonka
            && ((state.status == DealStatus::Releasing
                && state.released_total_ngonka < Uint256::from(state.total_claim_ngonka))
                || (state.status == DealStatus::Completed
                    && state.released_total_ngonka >= Uint256::from(state.total_claim_ngonka))) =>
        {
            Ok((*buyer_share_numerator, *share_denominator))
        }
        GnkReleasePolicy::HostOnly
            if state.total_claim_ngonka.is_zero()
                && matches!(
                    state.status,
                    DealStatus::Completed | DealStatus::Refunded | DealStatus::Expired
                ) =>
        {
            Ok((Uint128::zero(), Uint128::one()))
        }
        _ => Err(ContractError::InconsistentReleasePolicy),
    }
}

fn execute_settle_claim(deps: DepsMut, env: Env) -> Result<Response, ContractError> {
    let config = CONFIG.load(deps.storage)?;
    let mut state = STATE.load(deps.storage)?;
    if state.status != DealStatus::Locked {
        return Err(ContractError::InvalidSettlementState {
            actual: state.status,
        });
    }
    if !state.recipient_locked {
        return Err(ContractError::MissingRecipientLockProof);
    }

    let performance = query_epoch_performance(deps.as_ref(), &config.host, config.target_epoch)?;
    if !performance.claimed {
        return Err(ContractError::ClaimNotConfirmed {
            epoch: config.target_epoch,
        });
    }

    let buyer_funding = match state.buyer.as_ref() {
        Some(_) => BuyerFunding::Funded {
            actual_funded_budget_micro_usdt: config.buyer_budget_micro_usdt,
        },
        None => BuyerFunding::NoBuyer,
    };
    let settlement = calculate_claim_settlement(
        OfferTerms {
            configured_budget_micro_usdt: config.buyer_budget_micro_usdt,
            price_micro_usdt_per_gnk: config.price_micro_usdt_per_gnk,
        },
        buyer_funding,
        performance.work_amount,
        performance.reward_amount,
    )?;

    let entitlements = settlement.entitlements;
    let usdt = settlement.usdt;
    let (buyer_share_numerator, share_denominator, release_policy) =
        if entitlements.total_claim_ngonka.is_zero() {
            (Uint128::zero(), Uint128::one(), GnkReleasePolicy::HostOnly)
        } else {
            (
                entitlements.buyer_entitlement_ngonka,
                entitlements.total_claim_ngonka,
                GnkReleasePolicy::Proportional {
                    buyer_share_numerator: entitlements.buyer_entitlement_ngonka,
                    share_denominator: entitlements.total_claim_ngonka,
                },
            )
        };
    let host_share_numerator = share_denominator
        .checked_sub(buyer_share_numerator)
        .map_err(|_| ContractError::InconsistentReleasePolicy)?;
    if !usdt.buyer_refund_micro_usdt.is_zero() && state.buyer.is_none() {
        return Err(ContractError::SettlementRefundWithoutBuyer);
    }
    state.status = if entitlements.total_claim_ngonka.is_zero() {
        DealStatus::Completed
    } else {
        DealStatus::Releasing
    };
    state.work_ngonka = entitlements.work_ngonka;
    state.reward_ngonka = entitlements.reward_ngonka;
    state.total_claim_ngonka = entitlements.total_claim_ngonka;
    state.buyer_entitlement_ngonka = entitlements.buyer_entitlement_ngonka;
    state.host_entitlement_ngonka = entitlements.host_entitlement_ngonka;
    state.gnk_release_policy = release_policy;
    state.released_total_ngonka = Uint256::zero();
    state.buyer_released_ngonka = Uint256::zero();
    state.host_released_ngonka = Uint256::zero();
    state.gross_usdt = usdt.gross_micro_usdt;
    state.fee_usdt = usdt.fee_micro_usdt;
    state.host_net_usdt = usdt.host_net_micro_usdt;
    state.buyer_refund_usdt = usdt.buyer_refund_micro_usdt;

    // No external calls: settlement finality does not depend on token delivery.
    PENDING_USDT.save(
        deps.storage,
        &PendingUsdt {
            host: state.host_net_usdt,
            fee: state.fee_usdt,
            buyer: state.buyer_refund_usdt,
        },
    )?;
    STATE.save(deps.storage, &state)?;

    let mut response = Response::new();
    let mut settled_event = Event::new("claim_settled")
        .add_attribute("deal", env.contract.address.clone())
        .add_attribute("host", config.host.clone())
        .add_attribute("target_epoch", config.target_epoch.to_string())
        .add_attribute("work_ngonka", entitlements.work_ngonka)
        .add_attribute("reward_ngonka", entitlements.reward_ngonka)
        .add_attribute("total_claim_ngonka", entitlements.total_claim_ngonka)
        .add_attribute(
            "buyer_entitlement_ngonka",
            entitlements.buyer_entitlement_ngonka,
        )
        .add_attribute(
            "host_entitlement_ngonka",
            entitlements.host_entitlement_ngonka,
        )
        .add_attribute("buyer_share_numerator", buyer_share_numerator)
        .add_attribute("host_share_numerator", host_share_numerator)
        .add_attribute("share_denominator", share_denominator)
        .add_attribute("gross_usdt", usdt.gross_micro_usdt)
        .add_attribute("fee_usdt", usdt.fee_micro_usdt)
        .add_attribute("host_net_usdt", usdt.host_net_micro_usdt)
        .add_attribute("buyer_refund_usdt", usdt.buyer_refund_micro_usdt);
    if let Some(buyer) = state.buyer.as_ref() {
        settled_event = settled_event.add_attribute("buyer", buyer);
    }
    response = response.add_event(settled_event);

    if entitlements.total_claim_ngonka.is_zero() {
        response = response.add_event(deal_completed_event(&env, &config, Uint128::zero()));
    }

    Ok(response)
}

fn execute_withdraw_usdt(
    deps: DepsMut,
    env: Env,
    role: UsdtRole,
) -> Result<Response, ContractError> {
    let config = CONFIG.load(deps.storage)?;
    let mut pending = PENDING_USDT.may_load(deps.storage)?.unwrap_or_default();
    let (amount, recipient, payment) = match role {
        UsdtRole::Host => (&mut pending.host, Some(config.host.clone()), "host_net"),
        UsdtRole::Fee => (
            &mut pending.fee,
            Some(config.fee_recipient.clone()),
            "protocol_fee",
        ),
        UsdtRole::Buyer => (
            &mut pending.buyer,
            STATE.load(deps.storage)?.buyer,
            "buyer_refund",
        ),
    };
    if amount.is_zero() {
        return Err(ContractError::NothingToWithdraw);
    }
    let recipient = recipient.ok_or(ContractError::SettlementRefundWithoutBuyer)?;
    let due = *amount;
    *amount = Uint128::zero();
    // Checks-effects-interactions. Failed Transfer rolls back only this
    // transaction; a successful prior settlement or withdrawal is untouched.
    PENDING_USDT.save(deps.storage, &pending)?;
    let event = if role == UsdtRole::Buyer {
        Event::new("usdt_refunded")
            .add_attribute("deal", env.contract.address.clone())
            .add_attribute("host", config.host.clone())
            .add_attribute("target_epoch", config.target_epoch.to_string())
            .add_attribute("recipient", recipient.clone())
            .add_attribute("amount_micro_usdt", due)
    } else {
        usdt_paid_event(&env, &config, payment, &recipient, due)
    };
    Ok(Response::new()
        .add_message(cw20_transfer(&config.settlement_cw20, &recipient, due)?)
        .add_event(event))
}

fn query_usdt_payments(deps: Deps) -> StdResult<UsdtPaymentsResponse> {
    let config = CONFIG.load(deps.storage)?;
    let state = STATE.load(deps.storage)?;
    let pending = PENDING_USDT.may_load(deps.storage)?.unwrap_or_default();
    let payment = |recipient: Option<Addr>,
                   accrued: Uint128,
                   pending: Uint128|
     -> StdResult<UsdtPaymentResponse> {
        Ok(UsdtPaymentResponse {
            recipient: recipient.map(String::from),
            accrued_micro_usdt: accrued,
            paid_micro_usdt: accrued.checked_sub(pending).map_err(StdError::overflow)?,
            pending_micro_usdt: pending,
        })
    };
    Ok(UsdtPaymentsResponse {
        host: payment(Some(config.host), state.host_net_usdt, pending.host)?,
        fee: payment(Some(config.fee_recipient), state.fee_usdt, pending.fee)?,
        buyer: payment(state.buyer, state.buyer_refund_usdt, pending.buyer)?,
    })
}

fn deal_completed_event(env: &Env, config: &DealConfig, total_claim_ngonka: Uint128) -> Event {
    Event::new("deal_completed")
        .add_attribute("deal", env.contract.address.clone())
        .add_attribute("host", config.host.clone())
        .add_attribute("target_epoch", config.target_epoch.to_string())
        .add_attribute("total_claim_ngonka", total_claim_ngonka)
}

fn cw20_transfer(token: &Addr, recipient: &Addr, amount: Uint128) -> StdResult<WasmMsg> {
    Ok(WasmMsg::Execute {
        contract_addr: token.to_string(),
        msg: to_json_binary(&Cw20ExecuteMsg::Transfer {
            recipient: recipient.to_string(),
            amount,
        })?,
        funds: vec![],
    })
}

fn usdt_paid_event(
    env: &Env,
    config: &DealConfig,
    payment: &'static str,
    recipient: &Addr,
    amount: Uint128,
) -> Event {
    Event::new("usdt_paid")
        .add_attribute("deal", env.contract.address.clone())
        .add_attribute("host", config.host.clone())
        .add_attribute("target_epoch", config.target_epoch.to_string())
        .add_attribute("payment", payment)
        .add_attribute("recipient", recipient)
        .add_attribute("amount_micro_usdt", amount)
}

fn routing_window_end(target_epoch: u64) -> Result<u64, ContractError> {
    target_epoch
        .checked_add(CLAIM_RECIPIENT_PRUNING_THRESHOLD)
        .ok_or(ContractError::RoutingWindowOverflow {
            target: target_epoch,
        })
}

fn execute_lock(deps: DepsMut, env: Env) -> Result<Response, ContractError> {
    let config = CONFIG.load(deps.storage)?;
    let mut state = STATE.load(deps.storage)?;
    if !matches!(state.status, DealStatus::Open | DealStatus::Funded) {
        return Err(ContractError::InvalidLockState {
            actual: state.status,
        });
    }

    let current_epoch = query_current_epoch(&deps.querier)?;
    let end_exclusive = routing_window_end(config.target_epoch)?;
    if current_epoch < config.target_epoch {
        return Err(ContractError::LockWindowNotOpen {
            current: current_epoch,
            target: config.target_epoch,
        });
    }
    if current_epoch >= end_exclusive {
        return Err(ContractError::LockWindowClosed {
            current: current_epoch,
            target: config.target_epoch,
            end_exclusive,
        });
    }

    query_claim_recipient(
        deps.as_ref(),
        &config.host,
        config.target_epoch,
        &env.contract.address,
    )?;

    state.status = DealStatus::Locked;
    state.recipient_locked = true;
    STATE.save(deps.storage, &state)?;

    let mut event = Event::new("deal_locked")
        .add_attribute("deal", env.contract.address)
        .add_attribute("host", config.host)
        .add_attribute("target_epoch", config.target_epoch.to_string());
    if let Some(buyer) = state.buyer {
        event = event.add_attribute("buyer", buyer);
    }
    Ok(Response::new().add_event(event))
}

fn execute_cancel(deps: DepsMut, env: Env, info: MessageInfo) -> Result<Response, ContractError> {
    let config = CONFIG.load(deps.storage)?;
    let mut state = STATE.load(deps.storage)?;
    if state.status != DealStatus::Open {
        return Err(ContractError::InvalidCancelState {
            actual: state.status,
        });
    }
    if state.buyer.is_some() {
        return Err(ContractError::CannotCancelFundedDeal);
    }

    let current_epoch = query_current_epoch(&deps.querier)?;
    let end_exclusive = routing_window_end(config.target_epoch)?;
    if current_epoch < config.target_epoch {
        if info.sender != config.host {
            return Err(ContractError::CancelBeforeTargetRequiresHost {
                target: config.target_epoch,
            });
        }
    } else if current_epoch >= end_exclusive {
        return Err(ContractError::CancelWindowClosed {
            current: current_epoch,
            target: config.target_epoch,
            end_exclusive,
        });
    }

    match query_claim_recipient_routing(deps.as_ref(), &config.host, config.target_epoch)? {
        ClaimRecipientRouting::Missing => {}
        ClaimRecipientRouting::RoutedTo(recipient) if recipient != env.contract.address => {}
        ClaimRecipientRouting::RoutedTo(_) => {
            return Err(ContractError::ClaimRecipientStillRouted {
                epoch: config.target_epoch,
            });
        }
    }

    state.status = DealStatus::Cancelled;
    STATE.save(deps.storage, &state)?;

    Ok(Response::new().add_event(
        Event::new("deal_cancelled")
            .add_attribute("deal", env.contract.address)
            .add_attribute("host", config.host)
            .add_attribute("target_epoch", config.target_epoch.to_string()),
    ))
}

fn execute_refund(deps: DepsMut, env: Env) -> Result<Response, ContractError> {
    let config = CONFIG.load(deps.storage)?;
    let state = STATE.load(deps.storage)?;
    match state.status {
        DealStatus::Funded => execute_routing_refund(deps, env, config, state),
        DealStatus::Locked => execute_claim_expiry_refund(deps, env, config, state),
        actual => Err(ContractError::InvalidRefundState { actual }),
    }
}

fn execute_routing_refund(
    deps: DepsMut,
    env: Env,
    config: DealConfig,
    mut state: DealState,
) -> Result<Response, ContractError> {
    let buyer = state
        .buyer
        .clone()
        .ok_or(ContractError::RefundWithoutBuyer)?;
    validate_routing_refund_accounting(&state)?;
    let current_epoch = query_current_epoch(&deps.querier)?;
    let end_exclusive = routing_window_end(config.target_epoch)?;
    if current_epoch < config.target_epoch {
        return Err(ContractError::RefundWindowNotOpen {
            current: current_epoch,
            target: config.target_epoch,
        });
    }
    if current_epoch >= end_exclusive {
        return Err(ContractError::RefundWindowClosed {
            current: current_epoch,
            target: config.target_epoch,
            end_exclusive,
        });
    }

    let (refund_reason, observed_recipient) =
        match query_claim_recipient_routing(deps.as_ref(), &config.host, config.target_epoch)? {
            ClaimRecipientRouting::Missing => (RefundReason::RoutingMissing, None),
            ClaimRecipientRouting::RoutedTo(recipient) if recipient != env.contract.address => {
                (RefundReason::RoutingMismatch, Some(recipient))
            }
            ClaimRecipientRouting::RoutedTo(_) => {
                return Err(ContractError::RefundRecipientStillRouted {
                    epoch: config.target_epoch,
                });
            }
        };

    // A Funded Deal has never settled or released. Preserve its zero claim and
    // lifetime counters, freeze Host-only GNK ownership, and record the exact
    // accepted deposit rather than consulting the contract's live CW20 balance.
    state.status = DealStatus::Refunded;
    state.refund_reason = Some(refund_reason.clone());
    state.gnk_release_policy = GnkReleasePolicy::HostOnly;
    state.buyer_refund_usdt = config.buyer_budget_micro_usdt;
    STATE.save(deps.storage, &state)?;

    let reason = match refund_reason {
        RefundReason::RoutingMissing => "routing_missing",
        RefundReason::RoutingMismatch => "routing_mismatch",
        RefundReason::ClaimExpiry | RefundReason::NetworkUnconfirmed => {
            unreachable!("routing proof cannot yield a Locked refund reason")
        }
    };
    let mut refunded_event = Event::new("deal_refunded")
        .add_attribute("deal", env.contract.address.clone())
        .add_attribute("host", config.host.clone())
        .add_attribute("target_epoch", config.target_epoch.to_string())
        .add_attribute("buyer", buyer.clone())
        .add_attribute("reason", reason)
        .add_attribute("status", "refunded")
        .add_attribute("amount_micro_usdt", config.buyer_budget_micro_usdt);
    if let Some(recipient) = observed_recipient {
        refunded_event = refunded_event.add_attribute("observed_recipient", recipient);
    }

    Ok(Response::new()
        .add_message(cw20_transfer(
            &config.settlement_cw20,
            &buyer,
            config.buyer_budget_micro_usdt,
        )?)
        .add_event(refunded_event)
        .add_event(
            Event::new("usdt_refunded")
                .add_attribute("deal", env.contract.address)
                .add_attribute("host", config.host)
                .add_attribute("target_epoch", config.target_epoch.to_string())
                .add_attribute("recipient", buyer)
                .add_attribute("reason", reason)
                .add_attribute("amount_micro_usdt", config.buyer_budget_micro_usdt),
        ))
}

fn execute_claim_expiry_refund(
    deps: DepsMut,
    env: Env,
    config: DealConfig,
    mut state: DealState,
) -> Result<Response, ContractError> {
    validate_claim_expiry_accounting(&state)?;
    let current_epoch = query_current_epoch(&deps.querier)?;
    let first_allowed = config
        .target_epoch
        .checked_add(CLAIM_EXPIRY_DELAY_EPOCHS)
        .ok_or(ContractError::ClaimExpiryEpochOverflow {
            target: config.target_epoch,
        })?;
    let emergency_first_allowed = config
        .target_epoch
        .checked_add(NETWORK_UNCONFIRMED_DELAY_EPOCHS)
        .ok_or(ContractError::ClaimExpiryEpochOverflow {
            target: config.target_epoch,
        })?;
    if current_epoch < first_allowed {
        return Err(ContractError::ClaimExpiryWindowNotOpen {
            current: current_epoch,
            target: config.target_epoch,
            first_allowed,
        });
    }

    let refund_reason =
        match query_epoch_performance(deps.as_ref(), &config.host, config.target_epoch) {
            Ok(performance) => {
                if performance.claimed {
                    return Err(ContractError::ClaimAlreadyConfirmed {
                        epoch: config.target_epoch,
                    });
                }
                RefundReason::ClaimExpiry
            }
            Err(error)
                if current_epoch >= emergency_first_allowed
                    && error.permits_network_unconfirmed_refund() =>
            {
                RefundReason::NetworkUnconfirmed
            }
            Err(error) => return Err(error.into()),
        };
    let reason = match refund_reason {
        RefundReason::ClaimExpiry => "claim_expiry",
        RefundReason::NetworkUnconfirmed => "network_unconfirmed",
        RefundReason::RoutingMissing | RefundReason::RoutingMismatch => {
            unreachable!("Locked refund cannot use a routing reason")
        }
    };

    state.refund_reason = Some(refund_reason);
    state.gnk_release_policy = GnkReleasePolicy::HostOnly;

    if let Some(buyer) = state.buyer.clone() {
        state.status = DealStatus::Refunded;
        state.buyer_refund_usdt = config.buyer_budget_micro_usdt;

        // Write the terminal outcome before the external message. CosmWasm
        // transaction atomicity rolls this write back if the CW20 transfer
        // fails, so a permissionless retry cannot observe partial accounting.
        STATE.save(deps.storage, &state)?;

        Ok(Response::new()
            .add_message(cw20_transfer(
                &config.settlement_cw20,
                &buyer,
                config.buyer_budget_micro_usdt,
            )?)
            .add_event(
                Event::new("deal_refunded")
                    .add_attribute("deal", env.contract.address.clone())
                    .add_attribute("host", config.host.clone())
                    .add_attribute("target_epoch", config.target_epoch.to_string())
                    .add_attribute("buyer", buyer.clone())
                    .add_attribute("reason", reason)
                    .add_attribute("status", "refunded")
                    .add_attribute("amount_micro_usdt", config.buyer_budget_micro_usdt),
            )
            .add_event(
                Event::new("usdt_refunded")
                    .add_attribute("deal", env.contract.address)
                    .add_attribute("host", config.host)
                    .add_attribute("target_epoch", config.target_epoch.to_string())
                    .add_attribute("recipient", buyer)
                    .add_attribute("reason", reason)
                    .add_attribute("amount_micro_usdt", config.buyer_budget_micro_usdt),
            ))
    } else {
        state.status = DealStatus::Expired;
        STATE.save(deps.storage, &state)?;

        Ok(Response::new().add_event(
            Event::new("deal_expired")
                .add_attribute("deal", env.contract.address)
                .add_attribute("host", config.host)
                .add_attribute("target_epoch", config.target_epoch.to_string())
                .add_attribute("reason", reason)
                .add_attribute("status", "expired"),
        ))
    }
}

fn validate_routing_refund_accounting(state: &DealState) -> Result<(), ContractError> {
    if state.refund_reason.is_some()
        || state.recipient_locked
        || !state.work_ngonka.is_zero()
        || !state.reward_ngonka.is_zero()
        || !state.total_claim_ngonka.is_zero()
        || !state.buyer_entitlement_ngonka.is_zero()
        || !state.host_entitlement_ngonka.is_zero()
        || state.gnk_release_policy != GnkReleasePolicy::Unset
        || !state.released_total_ngonka.is_zero()
        || !state.buyer_released_ngonka.is_zero()
        || !state.host_released_ngonka.is_zero()
        || !state.gross_usdt.is_zero()
        || !state.fee_usdt.is_zero()
        || !state.host_net_usdt.is_zero()
        || !state.buyer_refund_usdt.is_zero()
    {
        return Err(ContractError::InconsistentRefundAccounting);
    }
    Ok(())
}

fn validate_claim_expiry_accounting(state: &DealState) -> Result<(), ContractError> {
    if state.refund_reason.is_some()
        || !state.recipient_locked
        || !state.work_ngonka.is_zero()
        || !state.reward_ngonka.is_zero()
        || !state.total_claim_ngonka.is_zero()
        || !state.buyer_entitlement_ngonka.is_zero()
        || !state.host_entitlement_ngonka.is_zero()
        || state.gnk_release_policy != GnkReleasePolicy::Unset
        || !state.released_total_ngonka.is_zero()
        || !state.buyer_released_ngonka.is_zero()
        || !state.host_released_ngonka.is_zero()
        || !state.gross_usdt.is_zero()
        || !state.fee_usdt.is_zero()
        || !state.host_net_usdt.is_zero()
        || !state.buyer_refund_usdt.is_zero()
    {
        return Err(ContractError::InconsistentRefundAccounting);
    }
    Ok(())
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Config {} => to_json_binary(&query_config(deps, env)?),
        QueryMsg::State {} => to_json_binary(&state_response(STATE.load(deps.storage)?)),
        QueryMsg::Funding {} => to_json_binary(&query_funding(deps)?),
        QueryMsg::Entitlements {} => to_json_binary(&query_entitlements(deps)?),
        QueryMsg::ReleaseStatus {} => to_json_binary(&query_release_status(deps)?),
        QueryMsg::UsdtPayments {} => to_json_binary(&query_usdt_payments(deps)?),
        QueryMsg::NativeStatus {} => to_json_binary(&query_native_status(deps, env)?),
    }
}

fn execute_receive(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    receive: Cw20ReceiveMsg,
) -> Result<Response, ContractError> {
    let config = CONFIG.load(deps.storage)?;
    if info.sender != config.settlement_cw20 {
        return Err(ContractError::WrongCw20);
    }

    let hook: Cw20HookMsg = from_json(receive.msg)?;
    let buyer = deps.api.addr_validate(&receive.sender)?;
    match hook {
        Cw20HookMsg::Fund {} => execute_fund(deps, env, config, buyer, receive.amount),
    }
}

fn execute_fund(
    deps: DepsMut,
    env: Env,
    config: DealConfig,
    buyer: Addr,
    amount: Uint128,
) -> Result<Response, ContractError> {
    let mut state = STATE.load(deps.storage)?;
    if state.status != DealStatus::Open {
        return Err(ContractError::InvalidFundingState {
            actual: state.status,
        });
    }
    if amount != config.buyer_budget_micro_usdt {
        return Err(ContractError::WrongFundingAmount {
            expected: config.buyer_budget_micro_usdt,
            actual: amount,
        });
    }

    let current_epoch = query_current_epoch(&deps.querier)?;
    if current_epoch >= config.target_epoch {
        return Err(ContractError::LateFunding {
            current: current_epoch,
            target: config.target_epoch,
        });
    }
    query_claim_recipient(
        deps.as_ref(),
        &config.host,
        config.target_epoch,
        &env.contract.address,
    )?;

    state.buyer = Some(buyer.clone());
    state.status = DealStatus::Funded;
    STATE.save(deps.storage, &state)?;

    Ok(Response::new().add_event(
        Event::new("deal_funded")
            .add_attribute("deal", env.contract.address)
            .add_attribute("host", config.host)
            .add_attribute("target_epoch", config.target_epoch.to_string())
            .add_attribute("buyer", buyer)
            .add_attribute("buyer_budget_micro_usdt", amount),
    ))
}

fn query_config(deps: Deps, env: Env) -> StdResult<ConfigResponse> {
    let config = CONFIG.load(deps.storage)?;
    Ok(ConfigResponse {
        factory: config.factory.to_string(),
        host: config.host.to_string(),
        deal_address: env.contract.address.to_string(),
        target_epoch: config.target_epoch,
        price_micro_usdt_per_gnk: config.price_micro_usdt_per_gnk,
        buyer_budget_micro_usdt: config.buyer_budget_micro_usdt,
        funded_capacity_ngonka: config.funded_capacity_ngonka,
        settlement_cw20: config.settlement_cw20.to_string(),
        fee_recipient: config.fee_recipient.to_string(),
        fee_bps: config.fee_bps,
        pinned_gonka_sha: config.pinned_gonka_sha,
    })
}

fn query_funding(deps: Deps) -> StdResult<FundingResponse> {
    let config = CONFIG.load(deps.storage)?;
    let state = STATE.load(deps.storage)?;
    Ok(FundingResponse {
        funded: state.buyer.is_some(),
        buyer: state.buyer.map(|buyer| buyer.to_string()),
        buyer_budget_micro_usdt: config.buyer_budget_micro_usdt,
        funded_capacity_ngonka: config.funded_capacity_ngonka,
    })
}

fn query_entitlements(deps: Deps) -> StdResult<EntitlementsResponse> {
    let state = STATE.load(deps.storage)?;
    Ok(EntitlementsResponse {
        work_ngonka: state.work_ngonka,
        reward_ngonka: state.reward_ngonka,
        total_claim_ngonka: state.total_claim_ngonka,
        buyer_entitlement_ngonka: state.buyer_entitlement_ngonka,
        host_entitlement_ngonka: state.host_entitlement_ngonka,
        gnk_release_policy: state.gnk_release_policy,
    })
}

fn query_release_status(deps: Deps) -> StdResult<ReleaseStatusResponse> {
    let state = STATE.load(deps.storage)?;
    let buyer_original_remaining_ngonka = original_entitlement_remaining(
        state.buyer_entitlement_ngonka,
        state.buyer_released_ngonka,
    )?;
    let host_original_remaining_ngonka =
        original_entitlement_remaining(state.host_entitlement_ngonka, state.host_released_ngonka)?;
    Ok(ReleaseStatusResponse {
        released_total_ngonka: state.released_total_ngonka,
        buyer_released_ngonka: state.buyer_released_ngonka,
        host_released_ngonka: state.host_released_ngonka,
        buyer_original_remaining_ngonka,
        host_original_remaining_ngonka,
        gnk_release_policy: state.gnk_release_policy,
    })
}

fn original_entitlement_remaining(
    original_entitlement: Uint128,
    lifetime_paid: Uint256,
) -> StdResult<Uint128> {
    if lifetime_paid >= Uint256::from(original_entitlement) {
        return Ok(Uint128::zero());
    }
    let paid_within_original = Uint128::try_from(lifetime_paid)
        .map_err(|_| StdError::generic_err("lifetime GNK counter conversion failed"))?;
    original_entitlement
        .checked_sub(paid_within_original)
        .map_err(|_| StdError::generic_err("original GNK remaining calculation failed"))
}

fn query_native_status(deps: Deps, env: Env) -> StdResult<NativeStatusResponse> {
    let remaining_vesting_ngonka = query_total_vesting(&deps.querier, &env.contract.address)
        .map_err(|error| StdError::generic_err(error.to_string()))?;
    let liquid_balance_ngonka = query_ngonka_balance(&deps.querier, &env.contract.address)
        .map_err(|error| StdError::generic_err(error.to_string()))?;
    Ok(NativeStatusResponse {
        remaining_vesting_ngonka,
        liquid_balance_ngonka,
    })
}

fn state_response(state: DealState) -> StateResponse {
    StateResponse {
        status: state.status,
        refund_reason: state.refund_reason,
        buyer: state.buyer.map(|buyer| buyer.to_string()),
        recipient_locked: state.recipient_locked,
        work_ngonka: state.work_ngonka,
        reward_ngonka: state.reward_ngonka,
        total_claim_ngonka: state.total_claim_ngonka,
        buyer_entitlement_ngonka: state.buyer_entitlement_ngonka,
        host_entitlement_ngonka: state.host_entitlement_ngonka,
        gnk_release_policy: state.gnk_release_policy,
        released_total_ngonka: state.released_total_ngonka,
        buyer_released_ngonka: state.buyer_released_ngonka,
        host_released_ngonka: state.host_released_ngonka,
        gross_usdt: state.gross_usdt,
        fee_usdt: state.fee_usdt,
        host_net_usdt: state.host_net_usdt,
        buyer_refund_usdt: state.buyer_refund_usdt,
    }
}

#[cfg(test)]
mod tests;
