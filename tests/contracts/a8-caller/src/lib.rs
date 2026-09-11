use cosmwasm_schema::{cw_serde, QueryResponses};
use cosmwasm_std::{
    entry_point, to_json_binary, Binary, Deps, DepsMut, Empty, Env, MessageInfo, Reply, ReplyOn,
    Response, StdError, StdResult, SubMsg, WasmMsg,
};
use cw_storage_plus::Item;

const REFUND_REPLY_ID: u64 = 1;
const LAST_REPLY: Item<ReplyResult> = Item::new("last_reply");

#[cw_serde]
pub struct InstantiateMsg {}

#[cw_serde]
pub enum ExecuteMsg {
    Refund { deal: String },
    RefundWithGas { deal: String, gas_limit: u64 },
}

#[cw_serde]
enum DealExecuteMsg {
    Refund {},
}

#[cw_serde]
#[derive(QueryResponses)]
pub enum QueryMsg {
    #[returns(ReplyResult)]
    LastReply {},
}

#[cw_serde]
pub struct ReplyResult {
    pub success: bool,
    pub error: Option<String>,
}

#[entry_point]
pub fn instantiate(
    deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    _msg: InstantiateMsg,
) -> StdResult<Response> {
    LAST_REPLY.save(
        deps.storage,
        &ReplyResult {
            success: false,
            error: None,
        },
    )?;
    Ok(Response::new().add_attribute("action", "a8_caller_instantiate"))
}

#[entry_point]
pub fn execute(
    deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    msg: ExecuteMsg,
) -> StdResult<Response> {
    match msg {
        ExecuteMsg::Refund { deal } => Ok(Response::new().add_message(refund_message(deps, deal)?)),
        ExecuteMsg::RefundWithGas { deal, gas_limit } => {
            if gas_limit == 0 {
                return Err(StdError::generic_err("gas_limit must be positive"));
            }
            LAST_REPLY.save(
                deps.storage,
                &ReplyResult {
                    success: false,
                    error: None,
                },
            )?;
            let submessage = SubMsg {
                id: REFUND_REPLY_ID,
                msg: refund_message(deps, deal)?.into(),
                gas_limit: Some(gas_limit),
                reply_on: ReplyOn::Always,
                payload: Binary::default(),
            };
            Ok(Response::new()
                .add_submessage(submessage)
                .add_attribute("action", "refund_with_gas")
                .add_attribute("gas_limit", gas_limit.to_string()))
        }
    }
}

fn refund_message(deps: DepsMut, deal: String) -> StdResult<WasmMsg> {
    let deal = deps.api.addr_validate(&deal)?;
    Ok(WasmMsg::Execute {
        contract_addr: deal.into_string(),
        msg: to_json_binary(&DealExecuteMsg::Refund {})?,
        funds: vec![],
    })
}

#[entry_point]
pub fn reply(deps: DepsMut, _env: Env, reply: Reply) -> StdResult<Response> {
    if reply.id != REFUND_REPLY_ID {
        return Err(StdError::generic_err("unexpected reply id"));
    }
    let result = match reply.result {
        cosmwasm_std::SubMsgResult::Ok(_) => ReplyResult {
            success: true,
            error: None,
        },
        cosmwasm_std::SubMsgResult::Err(error) => ReplyResult {
            success: false,
            error: Some(error),
        },
    };
    LAST_REPLY.save(deps.storage, &result)?;
    Ok(Response::new()
        .add_attribute("action", "refund_reply")
        .add_attribute("success", result.success.to_string()))
}

#[entry_point]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::LastReply {} => to_json_binary(&LAST_REPLY.load(deps.storage)?),
    }
}

#[entry_point]
pub fn migrate(_deps: DepsMut, _env: Env, _msg: Empty) -> StdResult<Response> {
    Err(StdError::generic_err(
        "test-only contract is not migratable",
    ))
}
