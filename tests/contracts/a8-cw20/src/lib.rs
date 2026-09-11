use cosmwasm_schema::cw_serde;
use cosmwasm_std::{
    entry_point, Addr, Binary, Deps, DepsMut, Env, MessageInfo, Response, StdError, StdResult,
    Uint128,
};
use cw20::Cw20ExecuteMsg;
use cw20_base::{
    contract,
    msg::{InstantiateMsg, QueryMsg},
    ContractError,
};
use cw_storage_plus::Item;

const FAULT: Item<FaultConfig> = Item::new("a8_fault");

#[cw_serde]
struct FaultConfig {
    controller: Addr,
    rejected_recipient: Option<Addr>,
}

#[cw_serde]
pub enum ExecuteMsg {
    Transfer {
        recipient: String,
        amount: Uint128,
    },
    Send {
        contract: String,
        amount: Uint128,
        msg: Binary,
    },
    ConfigureTransferFailure {
        rejected_recipient: Option<String>,
    },
}

#[entry_point]
pub fn instantiate(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    FAULT.save(
        deps.storage,
        &FaultConfig {
            controller: info.sender.clone(),
            rejected_recipient: None,
        },
    )?;
    contract::instantiate(deps, env, info, msg)
}

#[entry_point]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::Transfer { recipient, amount } => {
            let recipient_addr = deps.api.addr_validate(&recipient)?;
            let fault = FAULT.load(deps.storage)?;
            if fault.rejected_recipient.as_ref() == Some(&recipient_addr) {
                return Err(ContractError::Std(StdError::generic_err(format!(
                    "a8 injected CW20 transfer failure for {recipient_addr}"
                ))));
            }
            contract::execute(
                deps,
                env,
                info,
                Cw20ExecuteMsg::Transfer { recipient, amount },
            )
        }
        ExecuteMsg::Send {
            contract: recipient,
            amount,
            msg,
        } => contract::execute(
            deps,
            env,
            info,
            Cw20ExecuteMsg::Send {
                contract: recipient,
                amount,
                msg,
            },
        ),
        ExecuteMsg::ConfigureTransferFailure { rejected_recipient } => {
            let mut fault = FAULT.load(deps.storage)?;
            if info.sender != fault.controller {
                return Err(ContractError::Unauthorized {});
            }
            fault.rejected_recipient = rejected_recipient
                .map(|recipient| deps.api.addr_validate(&recipient))
                .transpose()?;
            FAULT.save(deps.storage, &fault)?;
            Ok(Response::new()
                .add_attribute("action", "configure_transfer_failure")
                .add_attribute(
                    "rejected_recipient",
                    fault
                        .rejected_recipient
                        .map_or_else(|| "none".to_owned(), |address| address.into_string()),
                ))
        }
    }
}

#[entry_point]
pub fn query(deps: Deps, env: Env, msg: QueryMsg) -> StdResult<Binary> {
    contract::query(deps, env, msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::testing::{message_info, mock_dependencies, mock_env};
    use cw20::{BalanceResponse, Cw20Coin};

    fn instantiate_token(deps: DepsMut, controller: &Addr, sender: &Addr) {
        instantiate(
            deps,
            mock_env(),
            message_info(controller, &[]),
            InstantiateMsg {
                name: "A8 token".to_owned(),
                symbol: "AUSDT".to_owned(),
                decimals: 6,
                initial_balances: vec![Cw20Coin {
                    address: sender.to_string(),
                    amount: Uint128::new(100),
                }],
                mint: None,
                marketing: None,
            },
        )
        .unwrap();
    }

    fn balance(deps: Deps, address: &str) -> Uint128 {
        let raw = query(
            deps,
            mock_env(),
            QueryMsg::Balance {
                address: address.to_owned(),
            },
        )
        .unwrap();
        cosmwasm_std::from_json::<BalanceResponse>(raw)
            .unwrap()
            .balance
    }

    #[test]
    fn configured_recipient_fails_closed_until_controller_clears_fault() {
        let mut deps = mock_dependencies();
        let controller_addr = deps.api.addr_make("controller");
        let sender = deps.api.addr_make("sender");
        let recipient = deps.api.addr_make("recipient");
        instantiate_token(deps.as_mut(), &controller_addr, &sender);
        let controller = message_info(&controller_addr, &[]);
        execute(
            deps.as_mut(),
            mock_env(),
            controller.clone(),
            ExecuteMsg::ConfigureTransferFailure {
                rejected_recipient: Some(recipient.to_string()),
            },
        )
        .unwrap();

        let error = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&sender, &[]),
            ExecuteMsg::Transfer {
                recipient: recipient.to_string(),
                amount: Uint128::new(10),
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("injected CW20 transfer failure"));
        assert_eq!(balance(deps.as_ref(), sender.as_str()), Uint128::new(100));
        assert_eq!(balance(deps.as_ref(), recipient.as_str()), Uint128::zero());

        execute(
            deps.as_mut(),
            mock_env(),
            controller,
            ExecuteMsg::ConfigureTransferFailure {
                rejected_recipient: None,
            },
        )
        .unwrap();
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&sender, &[]),
            ExecuteMsg::Transfer {
                recipient: recipient.to_string(),
                amount: Uint128::new(10),
            },
        )
        .unwrap();
        assert_eq!(balance(deps.as_ref(), sender.as_str()), Uint128::new(90));
        assert_eq!(balance(deps.as_ref(), recipient.as_str()), Uint128::new(10));
    }

    #[test]
    fn non_controller_cannot_configure_fault() {
        let mut deps = mock_dependencies();
        let controller = deps.api.addr_make("controller");
        let sender = deps.api.addr_make("sender");
        let attacker = deps.api.addr_make("attacker");
        let recipient = deps.api.addr_make("recipient");
        instantiate_token(deps.as_mut(), &controller, &sender);
        let error = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&attacker, &[]),
            ExecuteMsg::ConfigureTransferFailure {
                rejected_recipient: Some(recipient.to_string()),
            },
        )
        .unwrap_err();
        assert_eq!(error, ContractError::Unauthorized {});
    }
}
