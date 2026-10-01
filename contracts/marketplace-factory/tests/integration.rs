use cosmwasm_std::{
    coin, coins, to_json_binary, Addr, Api, BankMsg, BankQuery, Binary, BlockInfo, CustomMsg,
    CustomQuery, Deps, DepsMut, Empty, Env, GrpcQuery, MessageInfo, MigrateInfo, Querier, Response,
    StdError, StdResult, Storage, Uint128, Uint256,
};
use cw20::{BalanceResponse, Cw20Coin, Cw20ExecuteMsg, Cw20QueryMsg, Cw20ReceiveMsg};
use cw_multi_test::{
    error::AnyResult, AddressGenerator, AppBuilder, AppResponse, Bank, BankKeeper, BankSudo,
    Contract, ContractWrapper, CosmosRouter, Executor, IntoAddr, Module, SimpleAddressGenerator,
    Stargate, SudoMsg, WasmKeeper,
};
use cw_storage_plus::Item;
use gonka_proto::{
    ClaimRecipientEntry, Coin as ProtoCoin, EpochPerformanceSummary,
    QueryEpochPerformanceSummaryByParticipantRequest,
    QueryEpochPerformanceSummaryByParticipantResponse, QueryGetCurrentEpochRequest,
    QueryGetCurrentEpochResponse, QueryListClaimRecipientsRequest,
    QueryListClaimRecipientsResponse, QueryTotalVestingAmountRequest,
    QueryTotalVestingAmountResponse,
};
use marketplace_api::{deal, factory};
use prost::Message;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const FUNDING_BUDGET: u128 = 100_000_000;

fn factory_contract() -> Box<dyn Contract<Empty>> {
    Box::new(
        ContractWrapper::new(
            marketplace_factory::contract::execute,
            marketplace_factory::contract::instantiate,
            marketplace_factory::contract::query,
        )
        .with_reply(marketplace_factory::contract::reply),
    )
}

fn deal_contract() -> Box<dyn Contract<Empty>> {
    Box::new(ContractWrapper::new(
        marketplace_deal::contract::execute,
        marketplace_deal::contract::instantiate,
        marketplace_deal::contract::query,
    ))
}

fn failing_deal_contract() -> Box<dyn Contract<Empty>> {
    Box::new(ContractWrapper::new(
        migration_target_execute,
        failing_deal_instantiate,
        migration_target_query,
    ))
}

fn failing_deal_instantiate(
    _deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    _msg: deal::InstantiateMsg,
) -> StdResult<Response> {
    Err(StdError::generic_err("deliberate Deal instantiate failure"))
}

fn cw20_contract() -> Box<dyn Contract<Empty>> {
    Box::new(ContractWrapper::new(
        cw20_base::contract::execute,
        cw20_base::contract::instantiate,
        cw20_base::contract::query,
    ))
}

#[cosmwasm_schema::cw_serde]
struct FaultCw20InstantiateMsg {
    base: cw20_base::msg::InstantiateMsg,
    controller: String,
}

#[cosmwasm_schema::cw_serde]
enum FaultCw20ExecuteMsg {
    Transfer {
        recipient: String,
        amount: Uint128,
    },
    Send {
        contract: String,
        amount: Uint128,
        msg: Binary,
    },
    ConfigureFailure {
        fail_on_transfer: Option<u32>,
    },
    BlockRecipient {
        recipient: Option<String>,
    },
}

#[cosmwasm_schema::cw_serde]
struct FaultCw20Config {
    controller: Addr,
    fail_on_transfer: Option<u32>,
    transfer_count: u32,
    blocked_recipient: Option<String>,
}

const FAULT_CW20_CONFIG: Item<FaultCw20Config> = Item::new("fault_cw20_config");

fn fault_cw20_contract() -> Box<dyn Contract<Empty>> {
    Box::new(ContractWrapper::new(
        fault_cw20_execute,
        fault_cw20_instantiate,
        cw20_base::contract::query,
    ))
}

fn fault_cw20_instantiate(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: FaultCw20InstantiateMsg,
) -> Result<Response, cw20_base::ContractError> {
    let controller = deps.api.addr_validate(&msg.controller)?;
    FAULT_CW20_CONFIG.save(
        deps.storage,
        &FaultCw20Config {
            controller,
            fail_on_transfer: None,
            transfer_count: 0,
            blocked_recipient: None,
        },
    )?;
    cw20_base::contract::instantiate(deps, env, info, msg.base)
}

fn fault_cw20_execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: FaultCw20ExecuteMsg,
) -> Result<Response, cw20_base::ContractError> {
    match msg {
        FaultCw20ExecuteMsg::Transfer { recipient, amount } => {
            let config = FAULT_CW20_CONFIG.update(deps.storage, |mut config| {
                config.transfer_count = config
                    .transfer_count
                    .checked_add(1)
                    .ok_or_else(|| StdError::generic_err("fault CW20 transfer counter overflow"))?;
                Ok::<_, StdError>(config)
            })?;
            if config.blocked_recipient.as_ref() == Some(&recipient) {
                return Err(StdError::generic_err("recipient is blocked").into());
            }
            if config.fail_on_transfer == Some(config.transfer_count) {
                return Err(cw20_base::ContractError::Std(StdError::generic_err(
                    format!(
                        "deliberate CW20 transfer failure #{}",
                        config.transfer_count
                    ),
                )));
            }
            cw20_base::contract::execute(
                deps,
                env,
                info,
                cw20_base::msg::ExecuteMsg::Transfer { recipient, amount },
            )
        }
        FaultCw20ExecuteMsg::Send {
            contract,
            amount,
            msg,
        } => cw20_base::contract::execute(
            deps,
            env,
            info,
            cw20_base::msg::ExecuteMsg::Send {
                contract,
                amount,
                msg,
            },
        ),
        FaultCw20ExecuteMsg::BlockRecipient { recipient } => {
            FAULT_CW20_CONFIG.update(deps.storage, |mut config| {
                if info.sender != config.controller {
                    return Err(StdError::generic_err("only controller may block"));
                }
                config.blocked_recipient = recipient;
                Ok::<_, StdError>(config)
            })?;
            Ok(Response::new())
        }
        FaultCw20ExecuteMsg::ConfigureFailure { fail_on_transfer } => {
            FAULT_CW20_CONFIG.update(deps.storage, |mut config| {
                if info.sender != config.controller {
                    return Err(StdError::generic_err(
                        "only the fault CW20 controller may configure failures",
                    ));
                }
                config.fail_on_transfer = fail_on_transfer;
                config.transfer_count = 0;
                Ok::<_, StdError>(config)
            })?;
            Ok(Response::new())
        }
    }
}

#[derive(Clone, Debug, Default)]
struct BankFaultState {
    target_sender: Option<Addr>,
    fail_on_send: Option<u32>,
    matching_send_count: u32,
    fail_target_balance_query: bool,
}

struct FaultBankKeeper {
    inner: BankKeeper,
    state: Arc<Mutex<BankFaultState>>,
}

impl FaultBankKeeper {
    fn new(state: Arc<Mutex<BankFaultState>>) -> Self {
        Self {
            inner: BankKeeper::new(),
            state,
        }
    }
}

impl Bank for FaultBankKeeper {}

impl Module for FaultBankKeeper {
    type ExecT = BankMsg;
    type QueryT = BankQuery;
    type SudoT = BankSudo;

    fn execute<ExecC, QueryC>(
        &self,
        api: &dyn Api,
        storage: &mut dyn Storage,
        router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        block: &BlockInfo,
        sender: Addr,
        msg: BankMsg,
    ) -> AnyResult<AppResponse>
    where
        ExecC: CustomMsg + DeserializeOwned + 'static,
        QueryC: CustomQuery + DeserializeOwned + 'static,
    {
        if matches!(msg, BankMsg::Send { .. }) {
            let mut state = self.state.lock().unwrap();
            if state.target_sender.as_ref() == Some(&sender) {
                state.matching_send_count =
                    state.matching_send_count.checked_add(1).ok_or_else(|| {
                        cw_multi_test::error::anyhow!("fault bank send counter overflow")
                    })?;
                if state.fail_on_send == Some(state.matching_send_count) {
                    return Err(cw_multi_test::error::anyhow!(
                        "deliberate bank send failure #{}",
                        state.matching_send_count
                    ));
                }
            }
        }
        self.inner.execute(api, storage, router, block, sender, msg)
    }

    fn query(
        &self,
        api: &dyn Api,
        storage: &dyn Storage,
        querier: &dyn Querier,
        block: &BlockInfo,
        request: BankQuery,
    ) -> AnyResult<Binary> {
        if let BankQuery::Balance { address, denom: _ } = &request {
            let state = self.state.lock().unwrap();
            if state.fail_target_balance_query
                && state.target_sender.as_ref().map(Addr::as_str) == Some(address.as_str())
            {
                return Err(cw_multi_test::error::anyhow!(
                    "deliberate bank balance query failure"
                ));
            }
        }
        self.inner.query(api, storage, querier, block, request)
    }

    fn sudo<ExecC, QueryC>(
        &self,
        api: &dyn Api,
        storage: &mut dyn Storage,
        router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        block: &BlockInfo,
        msg: BankSudo,
    ) -> AnyResult<AppResponse>
    where
        ExecC: CustomMsg + DeserializeOwned + 'static,
        QueryC: CustomQuery + DeserializeOwned + 'static,
    {
        self.inner.sudo(api, storage, router, block, msg)
    }
}

fn migration_target_contract() -> Box<dyn Contract<Empty>> {
    Box::new(
        ContractWrapper::new(
            migration_target_execute,
            migration_target_instantiate,
            migration_target_query,
        )
        .with_migrate(migration_target_migrate),
    )
}

fn migration_target_instantiate(
    _deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    _msg: Empty,
) -> StdResult<Response> {
    Ok(Response::new())
}

fn migration_target_execute(
    _deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    _msg: Empty,
) -> StdResult<Response> {
    Ok(Response::new())
}

fn migration_target_query(_deps: Deps, _env: Env, _msg: Empty) -> StdResult<Binary> {
    to_json_binary(&Empty {})
}

fn migration_target_migrate(
    _deps: DepsMut,
    _env: Env,
    _msg: Empty,
    _info: MigrateInfo,
) -> StdResult<Response> {
    Ok(Response::new())
}

struct EpochStargate;

impl Stargate for EpochStargate {
    fn query_grpc(
        &self,
        _api: &dyn Api,
        _storage: &dyn Storage,
        _querier: &dyn Querier,
        _block: &BlockInfo,
        request: GrpcQuery,
    ) -> AnyResult<Binary> {
        if request.path != "/inference.inference.Query/GetCurrentEpoch" {
            return Err(cw_multi_test::error::anyhow!(
                "unexpected grpc path: {}",
                request.path
            ));
        }
        Ok(Binary::from(
            QueryGetCurrentEpochResponse { epoch: 10 }.encode_to_vec(),
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FundingQueryResponse {
    Valid,
    QueryFailure,
    Malformed,
    Oversized,
}

#[derive(Clone, Debug)]
struct FundingNativeState {
    current_epoch: u64,
    current_epoch_response: FundingQueryResponse,
    claim_recipients_response: FundingQueryResponse,
    performance_response: FundingQueryResponse,
    total_vesting_response: FundingQueryResponse,
    expected_host: Addr,
    expected_deal: Option<Addr>,
    claim_recipients: Vec<ClaimRecipientEntry>,
    performance_summary: Option<EpochPerformanceSummary>,
    total_vesting: Vec<ProtoCoin>,
}

#[derive(Clone)]
struct FundingStargate {
    state: Arc<Mutex<FundingNativeState>>,
}

impl FundingStargate {
    fn response(
        response: FundingQueryResponse,
        valid: impl FnOnce() -> Binary,
    ) -> AnyResult<Binary> {
        match response {
            FundingQueryResponse::Valid => Ok(valid()),
            FundingQueryResponse::QueryFailure => Err(cw_multi_test::error::anyhow!(
                "deliberate native query failure"
            )),
            FundingQueryResponse::Malformed => Ok(Binary::from(vec![0xff])),
            FundingQueryResponse::Oversized => Ok(Binary::from(vec![0; 32 * 1024 + 1])),
        }
    }
}

impl Stargate for FundingStargate {
    fn query_grpc(
        &self,
        _api: &dyn Api,
        _storage: &dyn Storage,
        _querier: &dyn Querier,
        _block: &BlockInfo,
        request: GrpcQuery,
    ) -> AnyResult<Binary> {
        let state = self.state.lock().unwrap();
        match request.path.as_str() {
            marketplace_common::gonka::GET_CURRENT_EPOCH_PATH => {
                QueryGetCurrentEpochRequest::decode(request.data.as_slice())?;
                Self::response(state.current_epoch_response, || {
                    Binary::from(
                        QueryGetCurrentEpochResponse {
                            epoch: state.current_epoch,
                        }
                        .encode_to_vec(),
                    )
                })
            }
            "/inference.inference.Query/ListClaimRecipients" => {
                let query = QueryListClaimRecipientsRequest::decode(request.data.as_slice())?;
                if query.participant != state.expected_host.as_str() {
                    return Err(cw_multi_test::error::anyhow!(
                        "unexpected participant: {}",
                        query.participant
                    ));
                }
                Self::response(state.claim_recipients_response, || {
                    Binary::from(
                        QueryListClaimRecipientsResponse {
                            entries: state.claim_recipients.clone(),
                        }
                        .encode_to_vec(),
                    )
                })
            }
            "/inference.inference.Query/EpochPerformanceSummaryByParticipant" => {
                let query = QueryEpochPerformanceSummaryByParticipantRequest::decode(
                    request.data.as_slice(),
                )?;
                if query.participant_id != state.expected_host.as_str() || query.epoch_index != 11 {
                    return Err(cw_multi_test::error::anyhow!(
                        "unexpected performance identity: {}/{}",
                        query.participant_id,
                        query.epoch_index
                    ));
                }
                Self::response(state.performance_response, || {
                    Binary::from(
                        QueryEpochPerformanceSummaryByParticipantResponse {
                            epoch_performance_summary: state.performance_summary.clone(),
                        }
                        .encode_to_vec(),
                    )
                })
            }
            "/inference.streamvesting.Query/TotalVestingAmount" => {
                let query = QueryTotalVestingAmountRequest::decode(request.data.as_slice())?;
                if state.expected_deal.as_ref().map(Addr::as_str)
                    != Some(query.participant_address.as_str())
                {
                    return Err(cw_multi_test::error::anyhow!(
                        "unexpected vesting participant: {}",
                        query.participant_address
                    ));
                }
                Self::response(state.total_vesting_response, || {
                    Binary::from(
                        QueryTotalVestingAmountResponse {
                            total_amount: state.total_vesting.clone(),
                        }
                        .encode_to_vec(),
                    )
                })
            }
            _ => Err(cw_multi_test::error::anyhow!(
                "unexpected grpc path: {}",
                request.path
            )),
        }
    }
}

#[derive(Clone, Debug)]
struct MultiDealNativeState {
    current_epoch: u64,
    claim_recipients: BTreeMap<String, Vec<ClaimRecipientEntry>>,
    performance_summaries: BTreeMap<(String, u64), EpochPerformanceSummary>,
}

#[derive(Clone)]
struct MultiDealStargate {
    state: Arc<Mutex<MultiDealNativeState>>,
}

impl Stargate for MultiDealStargate {
    fn query_grpc(
        &self,
        _api: &dyn Api,
        _storage: &dyn Storage,
        _querier: &dyn Querier,
        _block: &BlockInfo,
        request: GrpcQuery,
    ) -> AnyResult<Binary> {
        let state = self.state.lock().unwrap();
        match request.path.as_str() {
            marketplace_common::gonka::GET_CURRENT_EPOCH_PATH => {
                QueryGetCurrentEpochRequest::decode(request.data.as_slice())?;
                Ok(Binary::from(
                    QueryGetCurrentEpochResponse {
                        epoch: state.current_epoch,
                    }
                    .encode_to_vec(),
                ))
            }
            "/inference.inference.Query/ListClaimRecipients" => {
                let query = QueryListClaimRecipientsRequest::decode(request.data.as_slice())?;
                Ok(Binary::from(
                    QueryListClaimRecipientsResponse {
                        entries: state
                            .claim_recipients
                            .get(&query.participant)
                            .cloned()
                            .unwrap_or_default(),
                    }
                    .encode_to_vec(),
                ))
            }
            "/inference.inference.Query/EpochPerformanceSummaryByParticipant" => {
                let query = QueryEpochPerformanceSummaryByParticipantRequest::decode(
                    request.data.as_slice(),
                )?;
                Ok(Binary::from(
                    QueryEpochPerformanceSummaryByParticipantResponse {
                        epoch_performance_summary: state
                            .performance_summaries
                            .get(&(query.participant_id, query.epoch_index))
                            .cloned(),
                    }
                    .encode_to_vec(),
                ))
            }
            "/inference.streamvesting.Query/TotalVestingAmount" => {
                QueryTotalVestingAmountRequest::decode(request.data.as_slice())?;
                Ok(Binary::from(
                    QueryTotalVestingAmountResponse {
                        total_amount: vec![],
                    }
                    .encode_to_vec(),
                ))
            }
            _ => Err(cw_multi_test::error::anyhow!(
                "unexpected grpc path: {}",
                request.path
            )),
        }
    }
}

macro_rules! funding_fixture {
    () => {
        funding_fixture!(false)
    };
    ($fault_token:expr) => {{
        let owner = "owner".into_addr();
        let host = "host".into_addr();
        let buyer = "buyer".into_addr();
        let second_buyer = "second-buyer".into_addr();
        let native_state = Arc::new(Mutex::new(FundingNativeState {
            current_epoch: 10,
            current_epoch_response: FundingQueryResponse::Valid,
            claim_recipients_response: FundingQueryResponse::Valid,
            performance_response: FundingQueryResponse::Valid,
            total_vesting_response: FundingQueryResponse::Valid,
            expected_host: host.clone(),
            expected_deal: None,
            claim_recipients: vec![],
            performance_summary: Some(EpochPerformanceSummary {
                epoch_index: 11,
                participant_id: host.to_string(),
                claimed: true,
                ..EpochPerformanceSummary::default()
            }),
            total_vesting: vec![],
        }));
        let mut app = AppBuilder::new()
            .with_stargate(FundingStargate {
                state: native_state.clone(),
            })
            .build(|_, _, _| {});
        let cw20_code_id = if $fault_token {
            app.store_code(fault_cw20_contract())
        } else {
            app.store_code(cw20_contract())
        };
        let factory_code_id = app.store_code(factory_contract());
        let deal_code_id = app.store_code(deal_contract());
        let base_token_msg = cw20_base::msg::InstantiateMsg {
            name: "Test USDT".to_string(),
            symbol: "USDT".to_string(),
            decimals: 6,
            initial_balances: vec![
                Cw20Coin {
                    address: buyer.to_string(),
                    amount: Uint128::new(FUNDING_BUDGET * 3),
                },
                Cw20Coin {
                    address: second_buyer.to_string(),
                    amount: Uint128::new(FUNDING_BUDGET * 2),
                },
            ],
            mint: None,
            marketing: None,
        };
        let token = if $fault_token {
            app.instantiate_contract(
                cw20_code_id,
                owner.clone(),
                &FaultCw20InstantiateMsg {
                    base: base_token_msg,
                    controller: host.to_string(),
                },
                &[],
                "fault-test-usdt",
                None,
            )
            .unwrap()
        } else {
            app.instantiate_contract(
                cw20_code_id,
                owner.clone(),
                &base_token_msg,
                &[],
                "test-usdt",
                None,
            )
            .unwrap()
        };
        let factory_addr = app
            .instantiate_contract(
                factory_code_id,
                owner,
                &factory::InstantiateMsg {
                    deal_code_id,
                    settlement_cw20: token.to_string(),
                    fee_recipient: "fees".into_addr().to_string(),
                    fee_bps: 150,
                },
                &[],
                "marketplace-factory",
                None,
            )
            .unwrap();
        app.execute_contract(
            host.clone(),
            factory_addr.clone(),
            &factory::ExecuteMsg::CreateOffer {
                target_epoch: 11,
                price_micro_usdt_per_gnk: Uint128::new(1_000_000),
                buyer_budget_micro_usdt: Uint128::new(FUNDING_BUDGET),
            },
            &[],
        )
        .unwrap();
        let deal: factory::DealResponse = app
            .wrap()
            .query_wasm_smart(factory_addr.clone(), &factory::QueryMsg::Deal { id: 1 })
            .unwrap();
        let deal_addr = Addr::unchecked(deal.address);
        {
            let mut native = native_state.lock().unwrap();
            native.claim_recipients = vec![ClaimRecipientEntry {
                epoch: 11,
                recipient: deal_addr.to_string(),
            }];
            native.expected_deal = Some(deal_addr.clone());
        }
        (
            app,
            native_state,
            token,
            factory_addr,
            deal_addr,
            host,
            buyer,
            second_buyer,
        )
    }};
}

macro_rules! settlement_fixture_with_builder {
    ($builder:expr, $fault_token:expr, $funded:expr, $budget:expr, $price:expr, $host_label:expr, $buyer_label:expr, $fee_label:expr) => {{
        let owner = "owner".into_addr();
        let host = $host_label.into_addr();
        let buyer = $buyer_label.into_addr();
        let fee_recipient = $fee_label.into_addr();
        let native_state = Arc::new(Mutex::new(FundingNativeState {
            current_epoch: 10,
            current_epoch_response: FundingQueryResponse::Valid,
            claim_recipients_response: FundingQueryResponse::Valid,
            performance_response: FundingQueryResponse::Valid,
            total_vesting_response: FundingQueryResponse::Valid,
            expected_host: host.clone(),
            expected_deal: None,
            claim_recipients: vec![],
            performance_summary: Some(EpochPerformanceSummary {
                epoch_index: 11,
                participant_id: host.to_string(),
                claimed: true,
                ..EpochPerformanceSummary::default()
            }),
            total_vesting: vec![],
        }));
        let mut app = $builder
            .with_stargate(FundingStargate {
                state: native_state.clone(),
            })
            .build(|_, _, _| {});
        let token_code_id = if $fault_token {
            app.store_code(fault_cw20_contract())
        } else {
            app.store_code(cw20_contract())
        };
        let factory_code_id = app.store_code(factory_contract());
        let deal_code_id = app.store_code(deal_contract());
        let base_token_msg = cw20_base::msg::InstantiateMsg {
            name: "Test USDT".to_string(),
            symbol: "USDT".to_string(),
            decimals: 6,
            initial_balances: vec![Cw20Coin {
                address: buyer.to_string(),
                amount: Uint128::new($budget * 4),
            }],
            mint: None,
            marketing: None,
        };
        let token = if $fault_token {
            app.instantiate_contract(
                token_code_id,
                owner.clone(),
                &FaultCw20InstantiateMsg {
                    base: base_token_msg,
                    controller: owner.to_string(),
                },
                &[],
                "fault-test-usdt",
                None,
            )
            .unwrap()
        } else {
            app.instantiate_contract(
                token_code_id,
                owner.clone(),
                &base_token_msg,
                &[],
                "test-usdt",
                None,
            )
            .unwrap()
        };
        let factory_addr = app
            .instantiate_contract(
                factory_code_id,
                owner.clone(),
                &factory::InstantiateMsg {
                    deal_code_id,
                    settlement_cw20: token.to_string(),
                    fee_recipient: fee_recipient.to_string(),
                    fee_bps: 150,
                },
                &[],
                "marketplace-factory",
                None,
            )
            .unwrap();
        app.execute_contract(
            host.clone(),
            factory_addr.clone(),
            &factory::ExecuteMsg::CreateOffer {
                target_epoch: 11,
                price_micro_usdt_per_gnk: Uint128::new($price),
                buyer_budget_micro_usdt: Uint128::new($budget),
            },
            &[],
        )
        .unwrap();
        let deal: factory::DealResponse = app
            .wrap()
            .query_wasm_smart(factory_addr.clone(), &factory::QueryMsg::Deal { id: 1 })
            .unwrap();
        let deal_addr = Addr::unchecked(deal.address);
        {
            let mut native = native_state.lock().unwrap();
            native.claim_recipients = vec![ClaimRecipientEntry {
                epoch: 11,
                recipient: deal_addr.to_string(),
            }];
            native.expected_deal = Some(deal_addr.clone());
        }
        if $funded {
            app.execute_contract(
                buyer.clone(),
                token.clone(),
                &Cw20ExecuteMsg::Send {
                    contract: deal_addr.to_string(),
                    amount: Uint128::new($budget),
                    msg: to_json_binary(&deal::Cw20HookMsg::Fund {}).unwrap(),
                },
                &[],
            )
            .unwrap();
        }
        native_state.lock().unwrap().current_epoch = 11;
        app.execute_contract(
            "lock-keeper".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Lock {},
            &[],
        )
        .unwrap();
        (
            app,
            native_state,
            token,
            factory_addr,
            deal_addr,
            owner,
            host,
            buyer,
            fee_recipient,
        )
    }};
}

macro_rules! settlement_fixture {
    ($fault_token:expr, $funded:expr, $budget:expr, $price:expr, $host_label:expr, $buyer_label:expr, $fee_label:expr) => {{
        settlement_fixture_with_builder!(
            AppBuilder::new(),
            $fault_token,
            $funded,
            $budget,
            $price,
            $host_label,
            $buyer_label,
            $fee_label
        )
    }};
}

fn set_native_performance(
    native: &Arc<Mutex<FundingNativeState>>,
    epoch: u64,
    participant: String,
    earned_coins: u64,
    rewarded_coins: u64,
    claimed: bool,
) {
    native.lock().unwrap().performance_summary = Some(EpochPerformanceSummary {
        epoch_index: epoch,
        participant_id: participant,
        earned_coins,
        rewarded_coins,
        claimed,
        ..EpochPerformanceSummary::default()
    });
}

macro_rules! cw20_balance {
    ($app:expr, $token:expr, $address:expr) => {{
        $app.wrap()
            .query_wasm_smart::<BalanceResponse>(
                $token.clone(),
                &Cw20QueryMsg::Balance {
                    address: $address.to_string(),
                },
            )
            .unwrap()
            .balance
    }};
}

fn event_attribute<'a>(response: &'a AppResponse, event_type: &str, key: &str) -> Option<&'a str> {
    response
        .events
        .iter()
        .find(|event| event.ty == event_type)
        .and_then(|event| {
            event
                .attributes
                .iter()
                .find(|attribute| attribute.key == key)
        })
        .map(|attribute| attribute.value.as_str())
}

// Keep both outage scenarios subject to the same rollback checks. The macro
// accepts the concrete cw-multi-test app without duplicating its generic type.
macro_rules! assert_hidden_claim_preserves_state_and_balances {
    ($app:ident, $token:ident, $deal:ident, $buyer:ident, $host:ident, $fees:ident) => {{
        let before: deal::StateResponse = $app
            .wrap()
            .query_wasm_smart(&$deal, &deal::QueryMsg::State {})
            .unwrap();
        let balances = [
            cw20_balance!($app, $token, $deal),
            cw20_balance!($app, $token, $buyer),
            cw20_balance!($app, $token, $host),
            cw20_balance!($app, $token, $fees),
        ];
        for msg in [
            deal::ExecuteMsg::SettleClaim {},
            deal::ExecuteMsg::Refund {},
        ] {
            let error = $app
                .execute_contract("caller".into_addr(), $deal.clone(), &msg, &[])
                .unwrap_err();
            assert_eq!(
                error.root_cause().to_string(),
                "Gonka query failed for /inference.inference.Query/EpochPerformanceSummaryByParticipant"
            );
            assert_eq!(
                $app.wrap()
                    .query_wasm_smart::<deal::StateResponse>(&$deal, &deal::QueryMsg::State {})
                    .unwrap(),
                before
            );
            assert_eq!(
                [
                    cw20_balance!($app, $token, $deal),
                    cw20_balance!($app, $token, $buyer),
                    cw20_balance!($app, $token, $host),
                    cw20_balance!($app, $token, $fees),
                ],
                balances
            );
        }
        (before, balances)
    }};
}

macro_rules! withdraw_all_usdt {
    ($app:ident, $deal:ident) => {{
        let pending: deal::UsdtPaymentsResponse = $app
            .wrap()
            .query_wasm_smart($deal.clone(), &deal::QueryMsg::UsdtPayments {})
            .unwrap();
        let mut events = Vec::new();
        for (role, payment) in [
            (deal::UsdtRole::Host, pending.host),
            (deal::UsdtRole::Fee, pending.fee),
            (deal::UsdtRole::Buyer, pending.buyer),
        ] {
            if !payment.pending_micro_usdt.is_zero() {
                events.extend(
                    $app.execute_contract(
                        "payout-keeper".into_addr(),
                        $deal.clone(),
                        &deal::ExecuteMsg::WithdrawUsdt { role },
                        &[],
                    )
                    .unwrap()
                    .events,
                );
            }
        }
        events
    }};
}

#[test]
fn permanent_share_lifecycle_includes_pre_settlement_gifts_completion_and_late_alias() {
    const TOTAL: u128 = 100_000_000_000;
    const BUYER_SHARE: u128 = 80_000_000_000;
    let (mut app, native, token, factory_addr, deal_addr, _, host, buyer, _) =
        settlement_fixture!(false, true, 80_000_000, 1_000_000, "host", "buyer", "fees");
    // This gift exists before settlement. It cannot affect USDT settlement,
    // but becomes immediately distributable after the claim freezes shares.
    app.sudo(SudoMsg::Bank(BankSudo::Mint {
        to_address: deal_addr.to_string(),
        amount: vec![coin(25_000_000_000, "ngonka"), coin(77, "unrelated")],
    }))
    .unwrap();
    set_native_performance(&native, 11, host.to_string(), TOTAL as u64, 0, true);
    app.execute_contract(
        "settlement-keeper".into_addr(),
        deal_addr.clone(),
        &deal::ExecuteMsg::SettleClaim {},
        &[],
    )
    .unwrap();
    let frozen: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    assert_eq!(frozen.status, deal::DealStatus::Releasing);
    assert_eq!(frozen.total_claim_ngonka, Uint128::new(TOTAL));
    assert_eq!(frozen.buyer_entitlement_ngonka, Uint128::new(BUYER_SHARE));
    assert_eq!(
        frozen.host_entitlement_ngonka,
        Uint128::new(TOTAL - BUYER_SHARE)
    );
    assert_eq!(
        frozen.gnk_release_policy,
        deal::GnkReleasePolicy::Proportional {
            buyer_share_numerator: Uint128::new(BUYER_SHARE),
            share_denominator: Uint128::new(TOTAL),
        }
    );

    let first = app
        .execute_contract(
            "release-keeper-1".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::ReleaseUnlockedGnk {},
            &[],
        )
        .unwrap();
    assert_eq!(
        event_attribute(&first, "wasm-gnk_released", "released_total_ngonka"),
        Some("25000000000")
    );
    assert_eq!(
        app.wrap().query_balance(&buyer, "ngonka").unwrap().amount,
        Uint128::new(20_000_000_000)
    );
    assert_eq!(
        app.wrap().query_balance(&host, "ngonka").unwrap().amount,
        Uint128::new(5_000_000_000)
    );

    // Vesting is intentionally unavailable: release must depend only on the
    // spendable bank balance. This gift crosses T and is fully distributed.
    native.lock().unwrap().total_vesting_response = FundingQueryResponse::QueryFailure;
    app.sudo(SudoMsg::Bank(BankSudo::Mint {
        to_address: deal_addr.to_string(),
        amount: vec![coin(125_000_000_000, "ngonka")],
    }))
    .unwrap();
    let crossing = app
        .execute_contract(
            "release-keeper-2".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::ReleaseUnlockedGnk {},
            &[],
        )
        .unwrap();
    assert_eq!(
        event_attribute(&crossing, "wasm-deal_completed", "total_claim_ngonka"),
        Some("100000000000")
    );
    assert_eq!(
        event_attribute(&crossing, "wasm-gnk_released", "buyer_delta_ngonka"),
        Some("100000000000")
    );
    assert_eq!(
        event_attribute(&crossing, "wasm-gnk_released", "host_delta_ngonka"),
        Some("25000000000")
    );
    let completed: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    assert_eq!(completed.status, deal::DealStatus::Completed);
    assert_eq!(
        completed.released_total_ngonka,
        Uint256::from(150_000_000_000_u128)
    );
    assert_eq!(
        completed.buyer_released_ngonka,
        Uint256::from(120_000_000_000_u128)
    );
    assert_eq!(
        completed.host_released_ngonka,
        Uint256::from(30_000_000_000_u128)
    );
    assert_eq!(
        app.wrap().query_balance(&buyer, "ngonka").unwrap().amount,
        Uint128::new(120_000_000_000)
    );
    assert_eq!(
        app.wrap().query_balance(&host, "ngonka").unwrap().amount,
        Uint128::new(30_000_000_000)
    );
    assert_eq!(
        app.wrap()
            .query_balance(&deal_addr, "unrelated")
            .unwrap()
            .amount,
        Uint128::new(77)
    );
    withdraw_all_usdt!(app, deal_addr);
    assert_eq!(cw20_balance!(app, token, deal_addr), Uint128::zero());

    let indexed_completed: factory::DealResponse = app
        .wrap()
        .query_wasm_smart(
            factory_addr.clone(),
            &factory::QueryMsg::DealByHostEpoch {
                host: host.to_string(),
                epoch: 11,
            },
        )
        .unwrap();
    assert_eq!(indexed_completed.address, deal_addr.to_string());

    let no_liquidity = app
        .execute_contract(
            "repeat-release-keeper".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::ReleaseUnlockedGnk {},
            &[],
        )
        .unwrap_err();
    assert!(no_liquidity
        .root_cause()
        .to_string()
        .contains("no additional GNK"));

    app.sudo(SudoMsg::Bank(BankSudo::Mint {
        to_address: deal_addr.to_string(),
        amount: vec![coin(5_000_000_000, "ngonka")],
    }))
    .unwrap();
    let late_release = app
        .execute_contract(
            "late-release-keeper".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::ReleaseUnlockedGnk {},
            &[],
        )
        .unwrap();
    assert_eq!(
        event_attribute(&late_release, "wasm-gnk_released", "released_total_ngonka"),
        Some("155000000000")
    );
    assert!(late_release
        .events
        .iter()
        .all(|event| event.ty != "wasm-deal_completed"));

    app.sudo(SudoMsg::Bank(BankSudo::Mint {
        to_address: deal_addr.to_string(),
        amount: vec![coin(5_000_000_000, "ngonka")],
    }))
    .unwrap();
    let forwarded = app
        .execute_contract(
            "forward-keeper".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::ForwardExcessGnk {},
            &[],
        )
        .unwrap();
    assert_eq!(
        event_attribute(&forwarded, "wasm-excess_gnk_forwarded", "amount_ngonka"),
        Some("5000000000")
    );
    assert_eq!(
        event_attribute(
            &forwarded,
            "wasm-excess_gnk_forwarded",
            "buyer_delta_ngonka"
        ),
        Some("4000000000")
    );
    assert_eq!(
        app.wrap().query_balance(&buyer, "ngonka").unwrap().amount,
        Uint128::new(128_000_000_000)
    );
    assert_eq!(
        app.wrap().query_balance(&host, "ngonka").unwrap().amount,
        Uint128::new(32_000_000_000)
    );
    assert!(forwarded
        .events
        .iter()
        .all(|event| event.ty != "wasm-deal_completed"));
    let after_late: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    assert_eq!(
        after_late.released_total_ngonka,
        Uint256::from(160_000_000_000_u128)
    );
    let repeat = app
        .execute_contract(
            "repeat-forward-keeper".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::ForwardExcessGnk {},
            &[],
        )
        .unwrap_err();
    assert!(repeat
        .root_cause()
        .to_string()
        .contains("no available excess"));
    assert_eq!(
        app.wrap()
            .query_balance(&deal_addr, "unrelated")
            .unwrap()
            .amount,
        Uint128::new(77)
    );
    assert_eq!(cw20_balance!(app, token, deal_addr), Uint128::zero());
}

#[test]
fn release_supports_buyer_only_no_sale_and_coincident_recipients_without_zero_sends() {
    for (funded, host_label, buyer_label, claim, expected_buyer, expected_host) in [
        (
            true,
            "host-buyer-only",
            "buyer-only",
            52_000_000_000,
            52_000_000_000,
            0,
        ),
        (
            false,
            "host-no-sale",
            "unused-buyer",
            75_000_000_000,
            0,
            75_000_000_000,
        ),
        (
            true,
            "same-party",
            "same-party",
            200_000_000_000,
            100_000_000_000,
            100_000_000_000,
        ),
    ] {
        let (mut app, native, _, _, deal_addr, _, host, buyer, _) = settlement_fixture!(
            false,
            funded,
            FUNDING_BUDGET,
            1_000_000,
            host_label,
            buyer_label,
            "fees"
        );
        set_native_performance(&native, 11, host.to_string(), claim as u64, 0, true);
        app.execute_contract(
            "settlement-keeper".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::SettleClaim {},
            &[],
        )
        .unwrap();
        app.sudo(SudoMsg::Bank(BankSudo::Mint {
            to_address: deal_addr.to_string(),
            amount: vec![coin(claim, "ngonka")],
        }))
        .unwrap();
        let response = app
            .execute_contract(
                "release-keeper".into_addr(),
                deal_addr.clone(),
                &deal::ExecuteMsg::ReleaseUnlockedGnk {},
                &[],
            )
            .unwrap();
        let transfer_events = response
            .events
            .iter()
            .filter(|event| event.ty == "transfer")
            .count();
        let expected_nonzero_sides =
            usize::from(expected_buyer > 0) + usize::from(expected_host > 0);
        assert_eq!(transfer_events, expected_nonzero_sides);
        if host == buyer {
            assert_eq!(
                app.wrap().query_balance(&host, "ngonka").unwrap().amount,
                Uint128::new(expected_buyer + expected_host)
            );
        } else {
            assert_eq!(
                app.wrap().query_balance(&buyer, "ngonka").unwrap().amount,
                Uint128::new(expected_buyer)
            );
            assert_eq!(
                app.wrap().query_balance(&host, "ngonka").unwrap().amount,
                Uint128::new(expected_host)
            );
        }
        let state: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(deal_addr, &deal::QueryMsg::State {})
            .unwrap();
        assert_eq!(state.status, deal::DealStatus::Completed);
        assert_eq!(state.buyer_released_ngonka, Uint256::from(expected_buyer));
        assert_eq!(state.host_released_ngonka, Uint256::from(expected_host));
    }
}

#[test]
fn release_never_queries_vesting_even_when_every_vesting_response_shape_is_invalid() {
    for case in 0..5 {
        let (mut app, native, _, _, deal_addr, _, host, buyer, _) = settlement_fixture!(
            false,
            true,
            FUNDING_BUDGET,
            1_000_000,
            "host",
            "buyer",
            "fees"
        );
        set_native_performance(&native, 11, host.to_string(), 200_000_000_000, 0, true);
        app.execute_contract(
            "settlement-keeper".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::SettleClaim {},
            &[],
        )
        .unwrap();
        app.sudo(SudoMsg::Bank(BankSudo::Mint {
            to_address: deal_addr.to_string(),
            amount: vec![coin(20, "ngonka")],
        }))
        .unwrap();
        {
            let mut state = native.lock().unwrap();
            match case {
                0 => {
                    state.total_vesting_response = FundingQueryResponse::QueryFailure;
                }
                1 => {
                    state.total_vesting_response = FundingQueryResponse::Malformed;
                }
                2 => {
                    state.total_vesting = vec![ProtoCoin {
                        denom: "uatom".to_string(),
                        amount: "1".to_string(),
                    }];
                }
                3 => {
                    state.total_vesting = vec![
                        ProtoCoin {
                            denom: "ngonka".to_string(),
                            amount: "1".to_string(),
                        },
                        ProtoCoin {
                            denom: "ngonka".to_string(),
                            amount: "2".to_string(),
                        },
                    ];
                }
                _ => {
                    state.total_vesting = vec![ProtoCoin {
                        denom: "ngonka".to_string(),
                        amount: "not-an-amount".to_string(),
                    }];
                }
            }
        }
        let response = app
            .execute_contract(
                "release-keeper".into_addr(),
                deal_addr.clone(),
                &deal::ExecuteMsg::ReleaseUnlockedGnk {},
                &[],
            )
            .unwrap();
        assert_eq!(
            event_attribute(&response, "wasm-gnk_released", "available_balance_ngonka"),
            Some("20")
        );
        assert_eq!(
            app.wrap().query_balance(&buyer, "ngonka").unwrap().amount,
            Uint128::new(10)
        );
        assert_eq!(
            app.wrap().query_balance(&host, "ngonka").unwrap().amount,
            Uint128::new(10)
        );
    }
}

#[test]
fn zero_settlement_can_forward_late_ngonka_without_touching_cw20_or_other_denoms() {
    let (mut app, native, token, _, deal_addr, _, host, buyer, _) = settlement_fixture!(
        false,
        true,
        FUNDING_BUDGET,
        1_000_000,
        "zero-host",
        "zero-buyer",
        "zero-fees"
    );
    set_native_performance(&native, 11, host.to_string(), 0, 0, true);
    app.execute_contract(
        "settlement-keeper".into_addr(),
        deal_addr.clone(),
        &deal::ExecuteMsg::SettleClaim {},
        &[],
    )
    .unwrap();
    let completed: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    assert_eq!(completed.status, deal::DealStatus::Completed);
    assert_eq!(completed.total_claim_ngonka, Uint128::zero());
    let cw20_balances = [
        cw20_balance!(app, token, deal_addr),
        cw20_balance!(app, token, buyer),
        cw20_balance!(app, token, host),
    ];

    app.sudo(SudoMsg::Bank(BankSudo::Mint {
        to_address: deal_addr.to_string(),
        amount: vec![coin(9, "ngonka"), coin(13, "unrelated")],
    }))
    .unwrap();
    app.execute_contract(
        "forward-keeper".into_addr(),
        deal_addr.clone(),
        &deal::ExecuteMsg::ForwardExcessGnk {},
        &[],
    )
    .unwrap();
    assert_eq!(
        app.wrap().query_balance(&host, "ngonka").unwrap().amount,
        Uint128::new(9)
    );
    assert_eq!(
        app.wrap()
            .query_balance(&deal_addr, "unrelated")
            .unwrap()
            .amount,
        Uint128::new(13)
    );
    assert_eq!(
        [
            cw20_balance!(app, token, deal_addr),
            cw20_balance!(app, token, buyer),
            cw20_balance!(app, token, host),
        ],
        cw20_balances
    );
    let after_forward: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr, &deal::QueryMsg::State {})
        .unwrap();
    assert_eq!(after_forward.total_claim_ngonka, Uint128::zero());
    assert_eq!(after_forward.buyer_entitlement_ngonka, Uint128::zero());
    assert_eq!(after_forward.host_entitlement_ngonka, Uint128::zero());
    assert_eq!(after_forward.buyer_released_ngonka, Uint256::zero());
    assert_eq!(after_forward.host_released_ngonka, Uint256::from(9_u128));
    assert_eq!(after_forward.released_total_ngonka, Uint256::from(9_u128));
}

#[test]
fn bank_failures_roll_back_each_release_send_and_forwarding_then_allow_exact_retry() {
    const TOTAL: u128 = 200_000_000_000;
    for fail_on_send in [1, 2] {
        let bank_fault = Arc::new(Mutex::new(BankFaultState::default()));
        let (mut app, native, _, _, deal_addr, _, host, buyer, _) = settlement_fixture_with_builder!(
            AppBuilder::new().with_bank(FaultBankKeeper::new(bank_fault.clone())),
            false,
            true,
            FUNDING_BUDGET,
            1_000_000,
            "fault-host",
            "fault-buyer",
            "fault-fees"
        );
        set_native_performance(
            &native,
            11,
            host.to_string(),
            50_000_000_000,
            150_000_000_000,
            true,
        );
        app.execute_contract(
            "settlement-keeper".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::SettleClaim {},
            &[],
        )
        .unwrap();
        app.sudo(SudoMsg::Bank(BankSudo::Mint {
            to_address: deal_addr.to_string(),
            amount: vec![coin(TOTAL, "ngonka")],
        }))
        .unwrap();
        let state_before: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap();
        let balances_before = [
            app.wrap()
                .query_balance(&deal_addr, "ngonka")
                .unwrap()
                .amount,
            app.wrap().query_balance(&buyer, "ngonka").unwrap().amount,
            app.wrap().query_balance(&host, "ngonka").unwrap().amount,
        ];
        {
            let mut fault = bank_fault.lock().unwrap();
            fault.target_sender = Some(deal_addr.clone());
            fault.fail_on_send = Some(fail_on_send);
            fault.matching_send_count = 0;
        }
        let failure = app
            .execute_contract(
                "release-keeper".into_addr(),
                deal_addr.clone(),
                &deal::ExecuteMsg::ReleaseUnlockedGnk {},
                &[],
            )
            .unwrap_err();
        assert!(
            failure
                .root_cause()
                .to_string()
                .contains(&format!("deliberate bank send failure #{fail_on_send}")),
            "unexpected bank failure: {}",
            failure.root_cause()
        );
        assert_eq!(
            app.wrap()
                .query_wasm_smart::<deal::StateResponse>(
                    deal_addr.clone(),
                    &deal::QueryMsg::State {}
                )
                .unwrap(),
            state_before
        );
        assert_eq!(
            [
                app.wrap()
                    .query_balance(&deal_addr, "ngonka")
                    .unwrap()
                    .amount,
                app.wrap().query_balance(&buyer, "ngonka").unwrap().amount,
                app.wrap().query_balance(&host, "ngonka").unwrap().amount,
            ],
            balances_before,
            "failure #{fail_on_send} must roll back the ledger, including a successful first send"
        );

        if fail_on_send == 1 {
            {
                let mut fault = bank_fault.lock().unwrap();
                fault.fail_on_send = None;
                fault.matching_send_count = 0;
                fault.fail_target_balance_query = true;
            }
            let query_failure = app
                .execute_contract(
                    "release-keeper".into_addr(),
                    deal_addr.clone(),
                    &deal::ExecuteMsg::ReleaseUnlockedGnk {},
                    &[],
                )
                .unwrap_err();
            assert!(query_failure
                .root_cause()
                .to_string()
                .contains("Gonka query failed for bank/balance"));
            bank_fault.lock().unwrap().fail_target_balance_query = false;
            assert_eq!(
                app.wrap()
                    .query_wasm_smart::<deal::StateResponse>(
                        deal_addr.clone(),
                        &deal::QueryMsg::State {}
                    )
                    .unwrap(),
                state_before
            );
            assert_eq!(
                [
                    app.wrap()
                        .query_balance(&deal_addr, "ngonka")
                        .unwrap()
                        .amount,
                    app.wrap().query_balance(&buyer, "ngonka").unwrap().amount,
                    app.wrap().query_balance(&host, "ngonka").unwrap().amount,
                ],
                balances_before
            );
        }

        {
            let mut fault = bank_fault.lock().unwrap();
            fault.fail_on_send = None;
            fault.matching_send_count = 0;
        }
        app.execute_contract(
            "retry-keeper".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::ReleaseUnlockedGnk {},
            &[],
        )
        .unwrap();
        let completed: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap();
        assert_eq!(completed.status, deal::DealStatus::Completed);
        assert_eq!(completed.released_total_ngonka, Uint256::from(TOTAL));
        assert_eq!(
            app.wrap().query_balance(&buyer, "ngonka").unwrap().amount,
            Uint128::new(TOTAL / 2)
        );
        assert_eq!(
            app.wrap().query_balance(&host, "ngonka").unwrap().amount,
            Uint128::new(TOTAL / 2)
        );

        if fail_on_send == 1 {
            app.sudo(SudoMsg::Bank(BankSudo::Mint {
                to_address: deal_addr.to_string(),
                amount: vec![coin(7, "ngonka")],
            }))
            .unwrap();
            {
                let mut fault = bank_fault.lock().unwrap();
                fault.fail_on_send = Some(1);
                fault.matching_send_count = 0;
            }
            let before_forward_balances = [
                app.wrap()
                    .query_balance(&deal_addr, "ngonka")
                    .unwrap()
                    .amount,
                app.wrap().query_balance(&buyer, "ngonka").unwrap().amount,
                app.wrap().query_balance(&host, "ngonka").unwrap().amount,
            ];
            let forward_failure = app
                .execute_contract(
                    "forward-keeper".into_addr(),
                    deal_addr.clone(),
                    &deal::ExecuteMsg::ForwardExcessGnk {},
                    &[],
                )
                .unwrap_err();
            assert!(forward_failure
                .root_cause()
                .to_string()
                .contains("deliberate bank send failure #1"));
            assert_eq!(
                app.wrap()
                    .query_wasm_smart::<deal::StateResponse>(
                        deal_addr.clone(),
                        &deal::QueryMsg::State {}
                    )
                    .unwrap(),
                completed
            );
            assert_eq!(
                [
                    app.wrap()
                        .query_balance(&deal_addr, "ngonka")
                        .unwrap()
                        .amount,
                    app.wrap().query_balance(&buyer, "ngonka").unwrap().amount,
                    app.wrap().query_balance(&host, "ngonka").unwrap().amount,
                ],
                before_forward_balances
            );
            {
                let mut fault = bank_fault.lock().unwrap();
                fault.fail_on_send = None;
                fault.matching_send_count = 0;
            }
            app.execute_contract(
                "forward-retry-keeper".into_addr(),
                deal_addr.clone(),
                &deal::ExecuteMsg::ForwardExcessGnk {},
                &[],
            )
            .unwrap();
            assert_eq!(
                app.wrap()
                    .query_balance(&deal_addr, "ngonka")
                    .unwrap()
                    .amount,
                Uint128::zero()
            );
            assert_eq!(
                app.wrap().query_balance(&buyer, "ngonka").unwrap().amount,
                Uint128::new(TOTAL / 2 + 3)
            );
            assert_eq!(
                app.wrap().query_balance(&host, "ngonka").unwrap().amount,
                Uint128::new(TOTAL / 2 + 4)
            );
            let after_forward: deal::StateResponse = app
                .wrap()
                .query_wasm_smart(deal_addr, &deal::QueryMsg::State {})
                .unwrap();
            assert_eq!(after_forward.status, deal::DealStatus::Completed);
            assert_eq!(
                after_forward.released_total_ngonka,
                Uint256::from(TOTAL + 7)
            );
            assert_eq!(
                after_forward.buyer_released_ngonka,
                Uint256::from(TOTAL / 2 + 3)
            );
            assert_eq!(
                after_forward.host_released_ngonka,
                Uint256::from(TOTAL / 2 + 4)
            );
        }
    }
}

#[test]
fn different_release_partitions_beyond_claim_reach_identical_lifetime_accounting() {
    const LIFETIME_TOTAL: u128 = 250_000_000_000;
    for tranches in [
        vec![40_000_000_000, 160_000_000_000, 50_000_000_000],
        vec![
            1,
            59_999_999_999,
            10_000_000_000,
            130_000_000_000,
            50_000_000_000,
        ],
    ] {
        let (mut app, native, _, _, deal_addr, _, host, buyer, _) = settlement_fixture!(
            false,
            true,
            FUNDING_BUDGET,
            1_000_000,
            "partition-host",
            "partition-buyer",
            "partition-fees"
        );
        set_native_performance(
            &native,
            11,
            host.to_string(),
            50_000_000_000,
            150_000_000_000,
            true,
        );
        app.execute_contract(
            "settlement-keeper".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::SettleClaim {},
            &[],
        )
        .unwrap();

        let mut completion_events = 0;
        for tranche in tranches {
            app.sudo(SudoMsg::Bank(BankSudo::Mint {
                to_address: deal_addr.to_string(),
                amount: vec![coin(tranche, "ngonka")],
            }))
            .unwrap();
            let response = app
                .execute_contract(
                    "release-keeper".into_addr(),
                    deal_addr.clone(),
                    &deal::ExecuteMsg::ReleaseUnlockedGnk {},
                    &[],
                )
                .unwrap();
            completion_events += response
                .events
                .iter()
                .filter(|event| event.ty == "wasm-deal_completed")
                .count();
        }

        let state: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap();
        assert_eq!(state.status, deal::DealStatus::Completed);
        assert_eq!(completion_events, 1);
        assert_eq!(state.released_total_ngonka, Uint256::from(LIFETIME_TOTAL));
        assert_eq!(
            state.buyer_released_ngonka,
            Uint256::from(LIFETIME_TOTAL / 2)
        );
        assert_eq!(
            state.host_released_ngonka,
            Uint256::from(LIFETIME_TOTAL / 2)
        );
        assert_eq!(
            app.wrap().query_balance(&buyer, "ngonka").unwrap().amount,
            Uint128::new(LIFETIME_TOTAL / 2)
        );
        assert_eq!(
            app.wrap().query_balance(&host, "ngonka").unwrap().amount,
            Uint128::new(LIFETIME_TOTAL / 2)
        );
        assert_eq!(
            app.wrap()
                .query_balance(&deal_addr, "ngonka")
                .unwrap()
                .amount,
            Uint128::zero()
        );
    }
}

#[test]
fn release_and_forward_reject_wrong_states_and_native_funds_without_balance_changes() {
    let (mut app, native, token, _, deal_addr, _, host, buyer, _) = settlement_fixture!(
        false,
        true,
        FUNDING_BUDGET,
        1_000_000,
        "gate-host",
        "gate-buyer",
        "gate-fees"
    );
    app.sudo(SudoMsg::Bank(BankSudo::Mint {
        to_address: "caller".into_addr().to_string(),
        amount: vec![coin(2, "ngonka")],
    }))
    .unwrap();
    let locked_state: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    for msg in [
        deal::ExecuteMsg::ReleaseUnlockedGnk {},
        deal::ExecuteMsg::ForwardExcessGnk {},
    ] {
        let error = app
            .execute_contract("caller".into_addr(), deal_addr.clone(), &msg, &[])
            .unwrap_err();
        assert!(error.root_cause().to_string().contains("state Locked"));
        let funds_error = app
            .execute_contract(
                "caller".into_addr(),
                deal_addr.clone(),
                &msg,
                &[coin(1, "ngonka")],
            )
            .unwrap_err();
        assert!(funds_error.root_cause().to_string().contains("funds"));
    }
    assert_eq!(
        app.wrap()
            .query_wasm_smart::<deal::StateResponse>(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap(),
        locked_state
    );
    assert_eq!(
        cw20_balance!(app, token, deal_addr),
        Uint128::new(FUNDING_BUDGET)
    );
    assert_eq!(
        app.wrap().query_balance(&buyer, "ngonka").unwrap().amount,
        Uint128::zero()
    );
    assert_eq!(
        app.wrap().query_balance(&host, "ngonka").unwrap().amount,
        Uint128::zero()
    );

    set_native_performance(&native, 11, host.to_string(), 1, 0, true);
    app.execute_contract(
        "settlement-keeper".into_addr(),
        deal_addr.clone(),
        &deal::ExecuteMsg::SettleClaim {},
        &[],
    )
    .unwrap();
    let before_native_funds: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    let error = app
        .execute_contract(
            "caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::ReleaseUnlockedGnk {},
            &[coin(1, "ngonka")],
        )
        .unwrap_err();
    assert!(error.root_cause().to_string().contains("funds"));
    assert_eq!(
        app.wrap()
            .query_wasm_smart::<deal::StateResponse>(deal_addr, &deal::QueryMsg::State {})
            .unwrap(),
        before_native_funds
    );
}

#[test]
fn settle_claim_full_flow_covers_production_shapes_and_exact_conservation() {
    let whole_gnk = |amount: u64| amount * 1_000_000_000;
    let cases = [
        // earned, rewarded, buyer GNK, host GNK, gross, fee, host net, refund
        (2, 50, 52, 0, 52_000_000, 780_000, 51_220_000, 48_000_000),
        (10, 90, 100, 0, 100_000_000, 1_500_000, 98_500_000, 0),
        (100, 100, 100, 100, 100_000_000, 1_500_000, 98_500_000, 0),
        (99, 1, 100, 0, 100_000_000, 1_500_000, 98_500_000, 0),
        (90, 10, 100, 0, 100_000_000, 1_500_000, 98_500_000, 0),
    ];

    for (earned, rewarded, buyer_gnk, host_gnk, gross, fee, host_net, refund) in cases {
        let (mut app, native, token, _, deal_addr, _, host, buyer, fee_recipient) = settlement_fixture!(
            false,
            true,
            FUNDING_BUDGET,
            1_000_000,
            "host",
            "buyer",
            "fees"
        );
        set_native_performance(
            &native,
            11,
            host.to_string(),
            whole_gnk(earned),
            whole_gnk(rewarded),
            true,
        );

        let mut response = app
            .execute_contract(
                "permissionless-settler".into_addr(),
                deal_addr.clone(),
                &deal::ExecuteMsg::SettleClaim {},
                &[],
            )
            .unwrap();
        response.events.extend(withdraw_all_usdt!(app, deal_addr));
        let state: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap();
        assert_eq!(state.status, deal::DealStatus::Releasing);
        assert!(state.recipient_locked);
        assert_eq!(state.buyer, Some(buyer.to_string()));
        assert_eq!(state.work_ngonka, Uint128::from(whole_gnk(earned)));
        assert_eq!(state.reward_ngonka, Uint128::from(whole_gnk(rewarded)));
        assert_eq!(
            state.total_claim_ngonka,
            Uint128::from(whole_gnk(earned + rewarded))
        );
        assert_eq!(
            state.buyer_entitlement_ngonka,
            Uint128::from(whole_gnk(buyer_gnk))
        );
        assert_eq!(
            state.host_entitlement_ngonka,
            Uint128::from(whole_gnk(host_gnk))
        );
        assert_eq!(state.released_total_ngonka, Uint256::zero());
        assert_eq!(state.buyer_released_ngonka, Uint256::zero());
        assert_eq!(state.host_released_ngonka, Uint256::zero());
        assert_eq!(state.gross_usdt, Uint128::new(gross));
        assert_eq!(state.fee_usdt, Uint128::new(fee));
        assert_eq!(state.host_net_usdt, Uint128::new(host_net));
        assert_eq!(state.buyer_refund_usdt, Uint128::new(refund));
        assert_eq!(host_net + fee + refund, FUNDING_BUDGET);

        assert_eq!(cw20_balance!(app, token, host), Uint128::new(host_net));
        assert_eq!(cw20_balance!(app, token, fee_recipient), Uint128::new(fee));
        assert_eq!(
            cw20_balance!(app, token, buyer),
            Uint128::new(FUNDING_BUDGET * 3 + refund)
        );
        assert_eq!(cw20_balance!(app, token, deal_addr), Uint128::zero());
        assert!(response
            .events
            .iter()
            .any(|event| event.ty == "wasm-claim_settled"));
        assert_eq!(
            response
                .events
                .iter()
                .filter(|event| event.ty == "wasm-usdt_paid")
                .count(),
            2
        );
        assert_eq!(
            response
                .events
                .iter()
                .filter(|event| event.ty == "wasm-usdt_refunded")
                .count(),
            usize::from(refund > 0)
        );
    }
}

#[test]
fn settle_claim_handles_dust_and_suppresses_zero_transfers_and_events() {
    for (claim, expected_gross, expected_host_net, expected_refund) in
        [(1, 0, 0, 67), (66_000_000_000, 66, 66, 1)]
    {
        let (mut app, native, token, _, deal_addr, _, host, buyer, fee_recipient) =
            settlement_fixture!(false, true, 67, 1, "host", "buyer", "fees");
        set_native_performance(&native, 11, host.to_string(), claim, 0, true);
        let mut response = app
            .execute_contract(
                "caller".into_addr(),
                deal_addr.clone(),
                &deal::ExecuteMsg::SettleClaim {},
                &[],
            )
            .unwrap();
        response.events.extend(withdraw_all_usdt!(app, deal_addr));
        let state: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap();
        assert_eq!(state.gross_usdt, Uint128::new(expected_gross));
        assert_eq!(state.fee_usdt, Uint128::zero());
        assert_eq!(state.host_net_usdt, Uint128::new(expected_host_net));
        assert_eq!(state.buyer_refund_usdt, Uint128::new(expected_refund));
        assert_eq!(
            response
                .events
                .iter()
                .filter(|e| e.ty == "wasm-usdt_paid")
                .count(),
            usize::from(expected_host_net > 0)
        );
        assert!(!response.events.iter().any(|e| e
            .attributes
            .iter()
            .any(|a| a.key == "payment" && a.value == "protocol_fee")));
        assert_eq!(
            cw20_balance!(app, token, host),
            Uint128::new(expected_host_net)
        );
        assert_eq!(cw20_balance!(app, token, fee_recipient), Uint128::zero());
        assert_eq!(
            cw20_balance!(app, token, buyer),
            Uint128::new(201 + expected_refund)
        );
    }
}

#[test]
fn settle_claim_no_sale_and_zero_claim_follow_distinct_terminal_rules() {
    for (earned, rewarded, expected_status) in [
        (7, 5, deal::DealStatus::Releasing),
        (0, 0, deal::DealStatus::Completed),
    ] {
        let (mut app, native, token, _, deal_addr, _, host, buyer, fee_recipient) = settlement_fixture!(
            false,
            false,
            FUNDING_BUDGET,
            1_000_000,
            "host",
            "buyer",
            "fees"
        );
        set_native_performance(&native, 11, host.to_string(), earned, rewarded, true);
        let mut response = app
            .execute_contract(
                "caller".into_addr(),
                deal_addr.clone(),
                &deal::ExecuteMsg::SettleClaim {},
                &[],
            )
            .unwrap();
        response.events.extend(withdraw_all_usdt!(app, deal_addr));
        let state: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap();
        assert_eq!(state.status, expected_status);
        assert_eq!(state.buyer, None);
        assert_eq!(state.buyer_entitlement_ngonka, Uint128::zero());
        assert_eq!(
            state.host_entitlement_ngonka,
            Uint128::from(earned + rewarded)
        );
        assert_eq!(state.gross_usdt, Uint128::zero());
        assert_eq!(state.fee_usdt, Uint128::zero());
        assert_eq!(state.host_net_usdt, Uint128::zero());
        assert_eq!(state.buyer_refund_usdt, Uint128::zero());
        assert_eq!(cw20_balance!(app, token, deal_addr), Uint128::zero());
        assert_eq!(cw20_balance!(app, token, host), Uint128::zero());
        assert_eq!(cw20_balance!(app, token, fee_recipient), Uint128::zero());
        assert_eq!(
            cw20_balance!(app, token, buyer),
            Uint128::new(FUNDING_BUDGET * 4)
        );
        assert!(!response
            .events
            .iter()
            .any(|event| matches!(event.ty.as_str(), "wasm-usdt_paid" | "wasm-usdt_refunded")));
        assert_eq!(
            response
                .events
                .iter()
                .any(|event| event.ty == "wasm-deal_completed"),
            earned + rewarded == 0
        );
    }
}

#[test]
fn settlement_uses_locked_snapshot_and_deposit_accounting_not_live_rows_or_balance() {
    let (mut app, native, token, _, deal_addr, _, host, buyer, fee_recipient) = settlement_fixture!(
        false,
        true,
        FUNDING_BUDGET,
        1_000_000,
        "host",
        "buyer",
        "fees"
    );
    app.execute_contract(
        buyer.clone(),
        token.clone(),
        &Cw20ExecuteMsg::Transfer {
            recipient: deal_addr.to_string(),
            amount: Uint128::new(7_000_000),
        },
        &[],
    )
    .unwrap();
    {
        let mut state = native.lock().unwrap();
        state.current_epoch = 30;
        state.claim_recipients.clear();
    }
    set_native_performance(&native, 11, host.to_string(), 52_000_000_000, 0, true);
    let mut response = app
        .execute_contract(
            "late-permissionless-caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::SettleClaim {},
            &[],
        )
        .unwrap();
    response.events.extend(withdraw_all_usdt!(app, deal_addr));

    let state: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    assert_eq!(state.gross_usdt, Uint128::new(52_000_000));
    assert_eq!(state.host_net_usdt, Uint128::new(51_220_000));
    assert_eq!(state.fee_usdt, Uint128::new(780_000));
    assert_eq!(state.buyer_refund_usdt, Uint128::new(48_000_000));
    assert_eq!(
        cw20_balance!(app, token, deal_addr),
        Uint128::new(7_000_000)
    );
    assert_eq!(cw20_balance!(app, token, host), Uint128::new(51_220_000));
    assert_eq!(
        cw20_balance!(app, token, fee_recipient),
        Uint128::new(780_000)
    );
    assert_eq!(cw20_balance!(app, token, buyer), Uint128::new(341_000_000));
}

#[test]
fn settlement_aggregates_balance_changes_when_all_recipients_coincide() {
    let (mut app, native, token, _, deal_addr, _, host, buyer, fee_recipient) = settlement_fixture!(
        false,
        true,
        FUNDING_BUDGET,
        1_000_000,
        "same-recipient",
        "same-recipient",
        "same-recipient"
    );
    assert_eq!(host, buyer);
    assert_eq!(host, fee_recipient);
    set_native_performance(&native, 11, host.to_string(), 52_000_000_000, 0, true);
    let before = cw20_balance!(app, token, host);
    let mut response = app
        .execute_contract(
            "caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::SettleClaim {},
            &[],
        )
        .unwrap();
    response.events.extend(withdraw_all_usdt!(app, deal_addr));
    assert_eq!(before, Uint128::new(FUNDING_BUDGET * 3));
    assert_eq!(
        cw20_balance!(app, token, host),
        Uint128::new(FUNDING_BUDGET * 4)
    );
    assert_eq!(cw20_balance!(app, token, deal_addr), Uint128::zero());
    assert_eq!(
        response
            .events
            .iter()
            .filter(|event| event.ty == "wasm-usdt_paid")
            .count(),
        2
    );
    assert_eq!(
        response
            .events
            .iter()
            .filter(|event| event.ty == "wasm-usdt_refunded")
            .count(),
        1
    );
}

#[test]
fn settlement_native_summary_failures_preserve_locked_state_and_all_balances() {
    for case in 0..7 {
        let (mut app, native, token, _, deal_addr, _, host, buyer, fee_recipient) = settlement_fixture!(
            false,
            true,
            FUNDING_BUDGET,
            1_000_000,
            "host",
            "buyer",
            "fees"
        );
        let state_before: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap();
        let balances_before = [
            cw20_balance!(app, token, deal_addr),
            cw20_balance!(app, token, host),
            cw20_balance!(app, token, buyer),
            cw20_balance!(app, token, fee_recipient),
        ];
        let expected_error = match case {
            0 => {
                set_native_performance(&native, 11, host.to_string(), 1, 2, false);
                "not confirmed"
            }
            1 => {
                native.lock().unwrap().performance_summary = None;
                "missing required field epoch_performance_summary"
            }
            2 => {
                set_native_performance(&native, 12, host.to_string(), 1, 2, true);
                "mismatched epoch_index"
            }
            3 => {
                set_native_performance(
                    &native,
                    11,
                    "other-participant".into_addr().to_string(),
                    1,
                    2,
                    true,
                );
                "mismatched participant_id"
            }
            4 => {
                native.lock().unwrap().performance_response = FundingQueryResponse::Malformed;
                "failed to decode protobuf response"
            }
            5 => {
                native.lock().unwrap().performance_response = FundingQueryResponse::Oversized;
                "is too large"
            }
            _ => {
                native.lock().unwrap().performance_response = FundingQueryResponse::QueryFailure;
                "Gonka query failed"
            }
        };

        let error = app
            .execute_contract(
                "caller".into_addr(),
                deal_addr.clone(),
                &deal::ExecuteMsg::SettleClaim {},
                &[],
            )
            .unwrap_err();
        assert!(
            error.root_cause().to_string().contains(expected_error),
            "unexpected error for case {case}: {}",
            error.root_cause()
        );
        let state_after: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap();
        let balances_after = [
            cw20_balance!(app, token, deal_addr),
            cw20_balance!(app, token, host),
            cw20_balance!(app, token, buyer),
            cw20_balance!(app, token, fee_recipient),
        ];
        assert_eq!(state_after, state_before);
        assert_eq!(balances_after, balances_before);
    }
}

#[test]
fn settlement_rejects_open_funded_terminal_repeat_and_native_funds() {
    let (mut app, native, token, _, deal_addr, host, buyer, _) = funding_fixture!();
    set_native_performance(&native, 11, host.to_string(), 1, 0, true);
    let open_error = app
        .execute_contract(
            "caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::SettleClaim {},
            &[],
        )
        .unwrap_err();
    assert!(open_error
        .root_cause()
        .to_string()
        .contains("cannot settle claim in state Open"));

    app.execute_contract(
        buyer,
        token.clone(),
        &Cw20ExecuteMsg::Send {
            contract: deal_addr.to_string(),
            amount: Uint128::new(FUNDING_BUDGET),
            msg: to_json_binary(&deal::Cw20HookMsg::Fund {}).unwrap(),
        },
        &[],
    )
    .unwrap();
    let funded_error = app
        .execute_contract(
            "caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::SettleClaim {},
            &[],
        )
        .unwrap_err();
    assert!(funded_error
        .root_cause()
        .to_string()
        .contains("cannot settle claim in state Funded"));

    native.lock().unwrap().current_epoch = 11;
    app.execute_contract(
        "keeper".into_addr(),
        deal_addr.clone(),
        &deal::ExecuteMsg::Lock {},
        &[],
    )
    .unwrap();
    app.sudo(SudoMsg::Bank(BankSudo::Mint {
        to_address: "caller".into_addr().to_string(),
        amount: vec![coin(1, "ngonka")],
    }))
    .unwrap();
    let native_funds_error = app
        .execute_contract(
            "caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::SettleClaim {},
            &[coin(1, "ngonka")],
        )
        .unwrap_err();
    assert!(native_funds_error
        .root_cause()
        .to_string()
        .contains("funds"));

    set_native_performance(&native, 11, host.to_string(), 0, 0, true);
    app.execute_contract(
        "caller".into_addr(),
        deal_addr.clone(),
        &deal::ExecuteMsg::SettleClaim {},
        &[],
    )
    .unwrap();
    let balances_after = cw20_balance!(app, token, deal_addr);
    let repeat = app
        .execute_contract(
            "another-caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::SettleClaim {},
            &[],
        )
        .unwrap_err();
    assert!(repeat
        .root_cause()
        .to_string()
        .contains("cannot settle claim in state Completed"));
    assert_eq!(cw20_balance!(app, token, deal_addr), balances_after);
}

#[test]
fn blocked_recipient_cannot_block_settlement_other_roles_or_gnk_and_can_retry_after_completion() {
    for blocked in 0..3 {
        let (mut app, native, token, _, deal_addr, owner, host, buyer, fee_recipient) = settlement_fixture!(
            true,
            true,
            FUNDING_BUDGET,
            1_000_000,
            "host",
            "buyer",
            "fees"
        );
        set_native_performance(&native, 11, host.to_string(), 80_000_000_000, 0, true);
        let roles = [
            deal::UsdtRole::Host,
            deal::UsdtRole::Fee,
            deal::UsdtRole::Buyer,
        ];
        let recipients = [host.clone(), fee_recipient.clone(), buyer.clone()];
        let amounts = [78_800_000u128, 1_200_000, 20_000_000];
        app.execute_contract(
            owner.clone(),
            token.clone(),
            &FaultCw20ExecuteMsg::BlockRecipient {
                recipient: Some(recipients[blocked].to_string()),
            },
            &[],
        )
        .unwrap();
        let settled = app
            .execute_contract(
                "keeper".into_addr(),
                deal_addr.clone(),
                &deal::ExecuteMsg::SettleClaim {},
                &[],
            )
            .unwrap();
        assert!(!settled
            .events
            .iter()
            .any(|e| matches!(e.ty.as_str(), "wasm-usdt_paid" | "wasm-usdt_refunded")));
        assert_eq!(
            cw20_balance!(app, token, deal_addr),
            Uint128::new(FUNDING_BUDGET)
        );
        let frozen: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap();
        assert_eq!(frozen.status, deal::DealStatus::Releasing);
        // Try the blocked recipient before AND after paying every other role.
        for round in 0..2 {
            for (i, role) in roles.iter().enumerate() {
                let before: deal::UsdtPaymentsResponse = app
                    .wrap()
                    .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::UsdtPayments {})
                    .unwrap();
                let result = app.execute_contract(
                    "unrelated-keeper".into_addr(),
                    deal_addr.clone(),
                    &deal::ExecuteMsg::WithdrawUsdt { role: role.clone() },
                    &[],
                );
                if i == blocked || round == 1 {
                    let error = result.unwrap_err().root_cause().to_string();
                    assert!(error.contains(if i == blocked {
                        "recipient is blocked"
                    } else {
                        "no USDT remains payable"
                    }));
                    let after: deal::UsdtPaymentsResponse = app
                        .wrap()
                        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::UsdtPayments {})
                        .unwrap();
                    assert_eq!(before, after);
                } else {
                    result.unwrap();
                }
            }
        }
        let payments: deal::UsdtPaymentsResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::UsdtPayments {})
            .unwrap();
        for (i, payment) in [payments.host, payments.fee, payments.buyer]
            .iter()
            .enumerate()
        {
            assert_eq!(payment.accrued_micro_usdt, Uint128::new(amounts[i]));
            assert_eq!(
                payment.pending_micro_usdt,
                Uint128::new(if i == blocked { amounts[i] } else { 0 })
            );
            assert_eq!(
                payment.paid_micro_usdt + payment.pending_micro_usdt,
                payment.accrued_micro_usdt
            );
            let initial = if i == 2 { FUNDING_BUDGET * 3 } else { 0 };
            assert_eq!(
                cw20_balance!(app, token, recipients[i]),
                Uint128::new(initial) + payment.paid_micro_usdt
            );
        }
        assert_eq!(
            cw20_balance!(app, token, deal_addr),
            Uint128::new(amounts[blocked])
        );
        for msg in [
            deal::ExecuteMsg::SettleClaim {},
            deal::ExecuteMsg::Refund {},
        ] {
            assert!(app
                .execute_contract("keeper".into_addr(), deal_addr.clone(), &msg, &[])
                .is_err());
        }
        // Native data may disappear after settlement; payout retries never query it.
        native.lock().unwrap().performance_response = FundingQueryResponse::QueryFailure;
        app.sudo(SudoMsg::Bank(BankSudo::Mint {
            to_address: deal_addr.to_string(),
            amount: vec![coin(80_000_000_000, "ngonka")],
        }))
        .unwrap();
        app.execute_contract(
            "gnk-keeper".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::ReleaseUnlockedGnk {},
            &[],
        )
        .unwrap();
        let completed: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap();
        assert_eq!(completed.status, deal::DealStatus::Completed);
        assert_eq!(
            completed.buyer_released_ngonka,
            Uint256::from(80_000_000_000u128)
        );
        assert_eq!(
            app.wrap()
                .query_balance(buyer.clone(), "ngonka")
                .unwrap()
                .amount,
            Uint128::new(80_000_000_000)
        );
        assert_eq!(completed.gross_usdt, frozen.gross_usdt);
        app.execute_contract(
            owner,
            token.clone(),
            &FaultCw20ExecuteMsg::BlockRecipient { recipient: None },
            &[],
        )
        .unwrap();
        app.execute_contract(
            "retry-keeper".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::WithdrawUsdt {
                role: roles[blocked].clone(),
            },
            &[],
        )
        .unwrap();
        assert_eq!(cw20_balance!(app, token, deal_addr), Uint128::zero());
        assert_eq!(cw20_balance!(app, token, host), Uint128::new(amounts[0]));
        assert_eq!(
            cw20_balance!(app, token, fee_recipient),
            Uint128::new(amounts[1])
        );
        assert_eq!(
            cw20_balance!(app, token, buyer),
            Uint128::new(FUNDING_BUDGET * 3 + amounts[2])
        );
        assert!(app
            .execute_contract(
                "repeat".into_addr(),
                deal_addr.clone(),
                &deal::ExecuteMsg::WithdrawUsdt {
                    role: roles[blocked].clone()
                },
                &[]
            )
            .is_err());
        let after: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(deal_addr, &deal::QueryMsg::State {})
            .unwrap();
        assert_eq!(after, completed);
    }
}

#[test]
fn zero_claim_completes_despite_blocked_refund_and_withdraws_exactly_once() {
    let (mut app, native, token, _, deal_addr, owner, host, buyer, _) = settlement_fixture!(
        true,
        true,
        FUNDING_BUDGET,
        1_000_000,
        "host",
        "buyer",
        "fees"
    );
    set_native_performance(&native, 11, host.to_string(), 0, 0, true);
    app.execute_contract(
        owner.clone(),
        token.clone(),
        &FaultCw20ExecuteMsg::BlockRecipient {
            recipient: Some(buyer.to_string()),
        },
        &[],
    )
    .unwrap();
    app.execute_contract(
        "keeper".into_addr(),
        deal_addr.clone(),
        &deal::ExecuteMsg::SettleClaim {},
        &[],
    )
    .unwrap();
    let state: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    assert_eq!(state.status, deal::DealStatus::Completed);
    assert!(app
        .execute_contract(
            "keeper".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::WithdrawUsdt {
                role: deal::UsdtRole::Buyer
            },
            &[]
        )
        .is_err());
    assert_eq!(
        cw20_balance!(app, token, deal_addr),
        Uint128::new(FUNDING_BUDGET)
    );
    app.execute_contract(
        owner,
        token.clone(),
        &FaultCw20ExecuteMsg::BlockRecipient { recipient: None },
        &[],
    )
    .unwrap();
    withdraw_all_usdt!(app, deal_addr);
    assert_eq!(
        cw20_balance!(app, token, buyer),
        Uint128::new(FUNDING_BUDGET * 4)
    );
    assert_eq!(cw20_balance!(app, token, deal_addr), Uint128::zero());
    assert!(app
        .execute_contract(
            "keeper".into_addr(),
            deal_addr,
            &deal::ExecuteMsg::WithdrawUsdt {
                role: deal::UsdtRole::Buyer
            },
            &[]
        )
        .is_err());
}

macro_rules! assert_cw20_send_rolls_back {
    ($app:ident, $token:ident, $deal:ident, $sender:ident, $amount:expr, $hook:expr) => {{
        let buyer_before: BalanceResponse = $app
            .wrap()
            .query_wasm_smart(
                $token.clone(),
                &Cw20QueryMsg::Balance {
                    address: $sender.to_string(),
                },
            )
            .unwrap();
        let deal_before: BalanceResponse = $app
            .wrap()
            .query_wasm_smart(
                $token.clone(),
                &Cw20QueryMsg::Balance {
                    address: $deal.to_string(),
                },
            )
            .unwrap();
        let state_before: deal::StateResponse = $app
            .wrap()
            .query_wasm_smart($deal.clone(), &deal::QueryMsg::State {})
            .unwrap();

        let error = $app
            .execute_contract(
                $sender.clone(),
                $token.clone(),
                &Cw20ExecuteMsg::Send {
                    contract: $deal.to_string(),
                    amount: Uint128::new($amount),
                    msg: $hook,
                },
                &[],
            )
            .unwrap_err();

        let buyer_after: BalanceResponse = $app
            .wrap()
            .query_wasm_smart(
                $token.clone(),
                &Cw20QueryMsg::Balance {
                    address: $sender.to_string(),
                },
            )
            .unwrap();
        let deal_after: BalanceResponse = $app
            .wrap()
            .query_wasm_smart(
                $token.clone(),
                &Cw20QueryMsg::Balance {
                    address: $deal.to_string(),
                },
            )
            .unwrap();
        let state_after: deal::StateResponse = $app
            .wrap()
            .query_wasm_smart($deal.clone(), &deal::QueryMsg::State {})
            .unwrap();
        assert_eq!(buyer_after, buyer_before);
        assert_eq!(deal_after, deal_before);
        assert_eq!(state_after, state_before);
        error
    }};
}

#[test]
fn factory_to_deal_exact_cw20_send_funds_once_with_exact_balances() {
    let (mut app, _, token, _, deal_addr, host, buyer, _) = funding_fixture!();
    let config_before: deal::ConfigResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::Config {})
        .unwrap();

    let response = app
        .execute_contract(
            buyer.clone(),
            token.clone(),
            &Cw20ExecuteMsg::Send {
                contract: deal_addr.to_string(),
                amount: Uint128::new(FUNDING_BUDGET),
                msg: to_json_binary(&deal::Cw20HookMsg::Fund {}).unwrap(),
            },
            &[],
        )
        .unwrap();

    let funded_event = response
        .events
        .iter()
        .find(|event| event.ty == "wasm-deal_funded")
        .unwrap();
    for (key, value) in [
        ("deal", deal_addr.to_string()),
        ("host", host.to_string()),
        ("target_epoch", "11".to_string()),
        ("buyer", buyer.to_string()),
        ("buyer_budget_micro_usdt", FUNDING_BUDGET.to_string()),
    ] {
        assert!(funded_event
            .attributes
            .iter()
            .any(|attribute| attribute.key == key && attribute.value == value));
    }

    let state: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    assert_eq!(state.status, deal::DealStatus::Funded);
    assert_eq!(state.buyer, Some(buyer.to_string()));
    assert_eq!(state.total_claim_ngonka, Uint128::zero());
    let funding: deal::FundingResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::Funding {})
        .unwrap();
    assert!(funding.funded);
    assert_eq!(funding.buyer, Some(buyer.to_string()));
    assert_eq!(
        funding.buyer_budget_micro_usdt,
        Uint128::new(FUNDING_BUDGET)
    );
    assert_eq!(
        app.wrap()
            .query_wasm_smart::<deal::ConfigResponse>(
                deal_addr.clone(),
                &deal::QueryMsg::Config {},
            )
            .unwrap(),
        config_before
    );

    for (address, expected) in [
        (buyer, FUNDING_BUDGET * 2),
        (deal_addr.clone(), FUNDING_BUDGET),
        (host, 0),
        ("fees".into_addr(), 0),
    ] {
        let balance: BalanceResponse = app
            .wrap()
            .query_wasm_smart(
                token.clone(),
                &Cw20QueryMsg::Balance {
                    address: address.to_string(),
                },
            )
            .unwrap();
        assert_eq!(balance.balance, Uint128::new(expected));
    }
}

#[test]
fn rejected_real_cw20_sends_roll_back_transfer_and_open_state() {
    let exact_hook = || to_json_binary(&deal::Cw20HookMsg::Fund {}).unwrap();

    for amount in [FUNDING_BUDGET - 1, FUNDING_BUDGET + 1] {
        let (mut app, _, token, _, deal_addr, _, buyer, _) = funding_fixture!();
        assert_cw20_send_rolls_back!(app, token, deal_addr, buyer, amount, exact_hook());
    }

    let (mut app, _, token, _, deal_addr, _, buyer, _) = funding_fixture!();
    assert_cw20_send_rolls_back!(
        app,
        token,
        deal_addr,
        buyer,
        FUNDING_BUDGET,
        Binary::from(b"not-json")
    );

    #[derive(Clone, Copy)]
    enum NativeFailure {
        AtTargetEpoch,
        AfterTargetEpoch,
        MissingRecipient,
        MismatchedRecipient,
        DuplicateRecipient,
        RecipientQueryFailure,
        RecipientDecodeFailure,
        EpochQueryFailure,
        EpochDecodeFailure,
    }

    for failure in [
        NativeFailure::AtTargetEpoch,
        NativeFailure::AfterTargetEpoch,
        NativeFailure::MissingRecipient,
        NativeFailure::MismatchedRecipient,
        NativeFailure::DuplicateRecipient,
        NativeFailure::RecipientQueryFailure,
        NativeFailure::RecipientDecodeFailure,
        NativeFailure::EpochQueryFailure,
        NativeFailure::EpochDecodeFailure,
    ] {
        let (mut app, native, token, _, deal_addr, _, buyer, _) = funding_fixture!();
        {
            let mut state = native.lock().unwrap();
            match failure {
                NativeFailure::AtTargetEpoch => state.current_epoch = 11,
                NativeFailure::AfterTargetEpoch => state.current_epoch = 12,
                NativeFailure::MissingRecipient => state.claim_recipients.clear(),
                NativeFailure::MismatchedRecipient => {
                    state.claim_recipients = vec![ClaimRecipientEntry {
                        epoch: 11,
                        recipient: "other-deal".into_addr().to_string(),
                    }];
                }
                NativeFailure::DuplicateRecipient => {
                    state.claim_recipients.push(ClaimRecipientEntry {
                        epoch: 11,
                        recipient: deal_addr.to_string(),
                    });
                }
                NativeFailure::RecipientQueryFailure => {
                    state.claim_recipients_response = FundingQueryResponse::QueryFailure;
                }
                NativeFailure::RecipientDecodeFailure => {
                    state.claim_recipients_response = FundingQueryResponse::Malformed;
                }
                NativeFailure::EpochQueryFailure => {
                    state.current_epoch_response = FundingQueryResponse::QueryFailure;
                }
                NativeFailure::EpochDecodeFailure => {
                    state.current_epoch_response = FundingQueryResponse::Malformed;
                }
            }
        }
        assert_cw20_send_rolls_back!(app, token, deal_addr, buyer, FUNDING_BUDGET, exact_hook());
    }
}

#[test]
fn receive_authentication_state_and_native_funds_fail_closed() {
    let (mut app, _, token, _, deal_addr, _, buyer, _) = funding_fixture!();
    let hook = to_json_binary(&deal::Cw20HookMsg::Fund {}).unwrap();

    let direct_fake = app
        .execute_contract(
            buyer.clone(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Receive(Cw20ReceiveMsg {
                sender: buyer.to_string(),
                amount: Uint128::new(FUNDING_BUDGET),
                msg: hook.clone(),
            }),
            &[],
        )
        .unwrap_err();
    assert!(direct_fake
        .root_cause()
        .to_string()
        .contains("configured settlement token"));

    let wrong_token_code_id = app.store_code(cw20_contract());
    let wrong_token = app
        .instantiate_contract(
            wrong_token_code_id,
            buyer.clone(),
            &cw20_base::msg::InstantiateMsg {
                name: "Wrong USDT".to_string(),
                symbol: "WRONG".to_string(),
                decimals: 6,
                initial_balances: vec![Cw20Coin {
                    address: buyer.to_string(),
                    amount: Uint128::new(FUNDING_BUDGET),
                }],
                mint: None,
                marketing: None,
            },
            &[],
            "wrong-usdt",
            None,
        )
        .unwrap();
    assert_cw20_send_rolls_back!(
        app,
        wrong_token,
        deal_addr,
        buyer,
        FUNDING_BUDGET,
        hook.clone()
    );

    let invalid_buyer = app
        .execute_contract(
            token.clone(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Receive(Cw20ReceiveMsg {
                sender: "x".to_string(),
                amount: Uint128::new(FUNDING_BUDGET),
                msg: hook.clone(),
            }),
            &[],
        )
        .unwrap_err();
    let invalid_buyer_message = invalid_buyer.root_cause().to_string();
    assert!(
        invalid_buyer_message.contains("Error decoding bech32"),
        "unexpected invalid Buyer error: {invalid_buyer_message}"
    );

    app.sudo(SudoMsg::Bank(BankSudo::Mint {
        to_address: buyer.to_string(),
        amount: coins(1, "ngonka"),
    }))
    .unwrap();
    let native_funds = app
        .execute_contract(
            buyer.clone(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Receive(Cw20ReceiveMsg {
                sender: buyer.to_string(),
                amount: Uint128::new(FUNDING_BUDGET),
                msg: hook,
            }),
            &[coin(1, "ngonka")],
        )
        .unwrap_err();
    assert!(native_funds.root_cause().to_string().contains("funds"));
    assert_eq!(
        app.wrap().query_balance(&buyer, "ngonka").unwrap().amount,
        Uint128::one()
    );
    assert_eq!(
        app.wrap()
            .query_balance(&deal_addr, "ngonka")
            .unwrap()
            .amount,
        Uint128::zero()
    );

    let state: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr, &deal::QueryMsg::State {})
        .unwrap();
    assert_eq!(state.status, deal::DealStatus::Open);
    assert_eq!(state.buyer, None);
}

#[test]
fn repeat_funding_cannot_overwrite_or_appropriate_existing_deposit() {
    let (mut app, _, token, _, deal_addr, _, buyer, second_buyer) = funding_fixture!();
    let hook = to_json_binary(&deal::Cw20HookMsg::Fund {}).unwrap();
    app.execute_contract(
        buyer.clone(),
        token.clone(),
        &Cw20ExecuteMsg::Send {
            contract: deal_addr.to_string(),
            amount: Uint128::new(FUNDING_BUDGET),
            msg: hook.clone(),
        },
        &[],
    )
    .unwrap();

    for sender in [buyer.clone(), second_buyer.clone()] {
        assert_cw20_send_rolls_back!(app, token, deal_addr, sender, FUNDING_BUDGET, hook.clone());
    }

    let state: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    assert_eq!(state.status, deal::DealStatus::Funded);
    assert_eq!(state.buyer, Some(buyer.to_string()));
    let deal_balance: BalanceResponse = app
        .wrap()
        .query_wasm_smart(
            token.clone(),
            &Cw20QueryMsg::Balance {
                address: deal_addr.to_string(),
            },
        )
        .unwrap();
    assert_eq!(deal_balance.balance, Uint128::new(FUNDING_BUDGET));
    let second_balance: BalanceResponse = app
        .wrap()
        .query_wasm_smart(
            token,
            &Cw20QueryMsg::Balance {
                address: second_buyer.to_string(),
            },
        )
        .unwrap();
    assert_eq!(second_balance.balance, Uint128::new(FUNDING_BUDGET * 2));
}

#[test]
fn wrong_deal_state_rejects_real_send_and_rolls_back() {
    let (mut app, _, token, _, deal_addr, _, buyer, _) = funding_fixture!();
    let mut state: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    state.status = deal::DealStatus::Locked;
    {
        let mut storage = app.contract_storage_mut(&deal_addr);
        Item::<deal::StateResponse>::new("state")
            .save(&mut *storage, &state)
            .unwrap();
    }
    assert_cw20_send_rolls_back!(
        app,
        token,
        deal_addr,
        buyer,
        FUNDING_BUDGET,
        to_json_binary(&deal::Cw20HookMsg::Fund {}).unwrap()
    );
}

#[test]
fn factory_deals_lock_open_and_exact_funded_without_moving_assets() {
    for funded in [false, true] {
        let (mut app, native, token, _, deal_addr, host, buyer, second_buyer) = funding_fixture!();
        if funded {
            app.execute_contract(
                buyer.clone(),
                token.clone(),
                &Cw20ExecuteMsg::Send {
                    contract: deal_addr.to_string(),
                    amount: Uint128::new(FUNDING_BUDGET),
                    msg: to_json_binary(&deal::Cw20HookMsg::Fund {}).unwrap(),
                },
                &[],
            )
            .unwrap();
        }
        native.lock().unwrap().current_epoch = 11;

        let config_before: deal::ConfigResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::Config {})
            .unwrap();
        let state_before: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap();
        let buyer_before: BalanceResponse = app
            .wrap()
            .query_wasm_smart(
                token.clone(),
                &Cw20QueryMsg::Balance {
                    address: buyer.to_string(),
                },
            )
            .unwrap();
        let deal_before: BalanceResponse = app
            .wrap()
            .query_wasm_smart(
                token.clone(),
                &Cw20QueryMsg::Balance {
                    address: deal_addr.to_string(),
                },
            )
            .unwrap();

        if funded {
            let error = app
                .execute_contract(
                    host.clone(),
                    deal_addr.clone(),
                    &deal::ExecuteMsg::Cancel {},
                    &[],
                )
                .unwrap_err();
            assert!(error
                .root_cause()
                .to_string()
                .contains("cannot cancel Deal in state Funded; expected Open"));
        }

        let response = app
            .execute_contract(
                "keeper".into_addr(),
                deal_addr.clone(),
                &deal::ExecuteMsg::Lock {},
                &[],
            )
            .unwrap();
        let event = response
            .events
            .iter()
            .find(|event| event.ty == "wasm-deal_locked")
            .unwrap();
        for (key, value) in [
            ("deal", deal_addr.to_string()),
            ("host", host.to_string()),
            ("target_epoch", "11".to_string()),
        ] {
            assert!(event
                .attributes
                .iter()
                .any(|attribute| attribute.key == key && attribute.value == value));
        }

        let config_after: deal::ConfigResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::Config {})
            .unwrap();
        let state_after: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap();
        assert_eq!(config_after, config_before);
        assert_eq!(state_after.status, deal::DealStatus::Locked);
        assert!(state_after.recipient_locked);
        assert_eq!(state_after.buyer, state_before.buyer);
        assert_eq!(state_after.work_ngonka, state_before.work_ngonka);
        assert_eq!(state_after.reward_ngonka, state_before.reward_ngonka);
        assert_eq!(
            state_after.total_claim_ngonka,
            state_before.total_claim_ngonka
        );
        assert_eq!(state_after.gross_usdt, state_before.gross_usdt);

        let buyer_after: BalanceResponse = app
            .wrap()
            .query_wasm_smart(
                token.clone(),
                &Cw20QueryMsg::Balance {
                    address: buyer.to_string(),
                },
            )
            .unwrap();
        let deal_after: BalanceResponse = app
            .wrap()
            .query_wasm_smart(
                token.clone(),
                &Cw20QueryMsg::Balance {
                    address: deal_addr.to_string(),
                },
            )
            .unwrap();
        assert_eq!(buyer_after, buyer_before);
        assert_eq!(deal_after, deal_before);
        assert_eq!(
            app.wrap().query_balance(&host, "ngonka").unwrap().amount,
            Uint128::zero()
        );
        assert_eq!(
            app.wrap()
                .query_balance(&deal_addr, "ngonka")
                .unwrap()
                .amount,
            Uint128::zero()
        );

        let funding_error = assert_cw20_send_rolls_back!(
            app,
            token,
            deal_addr,
            second_buyer,
            FUNDING_BUDGET,
            to_json_binary(&deal::Cw20HookMsg::Fund {}).unwrap()
        );
        assert!(funding_error
            .root_cause()
            .to_string()
            .contains("cannot fund Deal in state Locked; expected Open"));
    }
}

#[test]
fn permissionless_cancel_keeps_factory_index_and_blocks_recreate_and_funding() {
    let (mut app, native, token, factory_addr, deal_addr, host, buyer, _) = funding_fixture!();
    {
        let mut state = native.lock().unwrap();
        state.current_epoch = 11;
        state.claim_recipients.clear();
    }
    let config_before: deal::ConfigResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::Config {})
        .unwrap();
    let buyer_before: BalanceResponse = app
        .wrap()
        .query_wasm_smart(
            token.clone(),
            &Cw20QueryMsg::Balance {
                address: buyer.to_string(),
            },
        )
        .unwrap();

    let response = app
        .execute_contract(
            "permissionless-caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Cancel {},
            &[],
        )
        .unwrap();
    let event = response
        .events
        .iter()
        .find(|event| event.ty == "wasm-deal_cancelled")
        .unwrap();
    for (key, value) in [
        ("deal", deal_addr.to_string()),
        ("host", host.to_string()),
        ("target_epoch", "11".to_string()),
    ] {
        assert!(event
            .attributes
            .iter()
            .any(|attribute| attribute.key == key && attribute.value == value));
    }

    let state: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    assert_eq!(state.status, deal::DealStatus::Cancelled);
    assert_eq!(state.buyer, None);
    assert!(!state.recipient_locked);
    assert_eq!(
        app.wrap()
            .query_wasm_smart::<deal::ConfigResponse>(
                deal_addr.clone(),
                &deal::QueryMsg::Config {},
            )
            .unwrap(),
        config_before
    );
    let indexed: factory::DealResponse = app
        .wrap()
        .query_wasm_smart(
            factory_addr.clone(),
            &factory::QueryMsg::DealByHostEpoch {
                host: host.to_string(),
                epoch: 11,
            },
        )
        .unwrap();
    assert_eq!(indexed.address, deal_addr.to_string());

    native.lock().unwrap().current_epoch = 10;
    let duplicate = app
        .execute_contract(
            host,
            factory_addr,
            &factory::ExecuteMsg::CreateOffer {
                target_epoch: 11,
                price_micro_usdt_per_gnk: Uint128::new(1_000_000),
                buyer_budget_micro_usdt: Uint128::new(FUNDING_BUDGET),
            },
            &[],
        )
        .unwrap_err();
    assert!(duplicate
        .root_cause()
        .to_string()
        .contains("already exists"));

    let funding_error = assert_cw20_send_rolls_back!(
        app,
        token,
        deal_addr,
        buyer,
        FUNDING_BUDGET,
        to_json_binary(&deal::Cw20HookMsg::Fund {}).unwrap()
    );
    assert!(funding_error
        .root_cause()
        .to_string()
        .contains("cannot fund Deal in state Cancelled; expected Open"));
    let buyer_after: BalanceResponse = app
        .wrap()
        .query_wasm_smart(
            token,
            &Cw20QueryMsg::Balance {
                address: buyer.to_string(),
            },
        )
        .unwrap();
    assert_eq!(buyer_after, buyer_before);
}

#[test]
fn routing_refund_returns_only_the_deposit_keeps_other_assets_and_enables_host_only_gnk() {
    let (mut app, native, token, factory_addr, deal_addr, host, buyer, donor) = funding_fixture!();
    app.execute_contract(
        buyer.clone(),
        token.clone(),
        &Cw20ExecuteMsg::Send {
            contract: deal_addr.to_string(),
            amount: Uint128::new(FUNDING_BUDGET),
            msg: to_json_binary(&deal::Cw20HookMsg::Fund {}).unwrap(),
        },
        &[],
    )
    .unwrap();
    app.execute_contract(
        donor.clone(),
        token.clone(),
        &Cw20ExecuteMsg::Transfer {
            recipient: deal_addr.to_string(),
            amount: Uint128::new(7_000_000),
        },
        &[],
    )
    .unwrap();
    app.sudo(SudoMsg::Bank(BankSudo::Mint {
        to_address: deal_addr.to_string(),
        amount: vec![coin(13, "ngonka"), coin(17, "uatom")],
    }))
    .unwrap();
    {
        let mut state = native.lock().unwrap();
        state.current_epoch = 11;
        state.claim_recipients.clear();
    }

    let response = app
        .execute_contract(
            "permissionless-refunder".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Refund {},
            &[],
        )
        .unwrap();
    assert_eq!(
        event_attribute(&response, "wasm-deal_refunded", "reason"),
        Some("routing_missing")
    );
    assert_eq!(
        event_attribute(&response, "wasm-deal_refunded", "status"),
        Some("refunded")
    );
    assert_eq!(
        event_attribute(&response, "wasm-deal_refunded", "amount_micro_usdt"),
        Some("100000000")
    );
    assert_eq!(
        response
            .events
            .iter()
            .filter(|event| event.ty == "wasm-usdt_paid")
            .count(),
        0
    );
    assert_eq!(
        cw20_balance!(app, token, buyer),
        Uint128::new(FUNDING_BUDGET * 3)
    );
    assert_eq!(
        cw20_balance!(app, token, donor),
        Uint128::new(FUNDING_BUDGET * 2 - 7_000_000)
    );
    assert_eq!(
        cw20_balance!(app, token, deal_addr),
        Uint128::new(7_000_000)
    );
    assert_eq!(cw20_balance!(app, token, host), Uint128::zero());
    assert_eq!(
        cw20_balance!(app, token, "fees".into_addr()),
        Uint128::zero()
    );

    let state: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    assert_eq!(state.status, deal::DealStatus::Refunded);
    assert_eq!(
        state.refund_reason,
        Some(deal::RefundReason::RoutingMissing)
    );
    assert_eq!(state.gnk_release_policy, deal::GnkReleasePolicy::HostOnly);
    assert_eq!(state.buyer_refund_usdt, Uint128::new(FUNDING_BUDGET));
    assert_eq!(state.gross_usdt, Uint128::zero());
    assert_eq!(state.fee_usdt, Uint128::zero());
    assert_eq!(state.host_net_usdt, Uint128::zero());
    assert_eq!(state.total_claim_ngonka, Uint128::zero());

    assert_eq!(state.released_total_ngonka, Uint256::zero());

    for msg in [
        deal::ExecuteMsg::Refund {},
        deal::ExecuteMsg::SettleClaim {},
    ] {
        let error = app
            .execute_contract("repeat-caller".into_addr(), deal_addr.clone(), &msg, &[])
            .unwrap_err();
        assert!(error.root_cause().to_string().contains("state Refunded"));
    }
    let alias_error = app
        .execute_contract(
            "caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::ForwardExcessGnk {},
            &[],
        )
        .unwrap_err();
    assert!(alias_error
        .root_cause()
        .to_string()
        .contains("expected Completed"));

    app.execute_contract(
        "release-caller".into_addr(),
        deal_addr.clone(),
        &deal::ExecuteMsg::ReleaseUnlockedGnk {},
        &[],
    )
    .unwrap();
    assert_eq!(
        app.wrap().query_balance(&host, "ngonka").unwrap().amount,
        Uint128::new(13)
    );
    assert_eq!(
        app.wrap()
            .query_balance(&deal_addr, "ngonka")
            .unwrap()
            .amount,
        Uint128::zero()
    );
    assert_eq!(
        app.wrap()
            .query_balance(&deal_addr, "uatom")
            .unwrap()
            .amount,
        Uint128::new(17)
    );
    assert_eq!(
        cw20_balance!(app, token, deal_addr),
        Uint128::new(7_000_000)
    );
    let release: deal::ReleaseStatusResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::ReleaseStatus {})
        .unwrap();
    assert_eq!(release.released_total_ngonka, Uint256::from(13u128));
    assert_eq!(release.buyer_released_ngonka, Uint256::zero());
    assert_eq!(release.host_released_ngonka, Uint256::from(13u128));

    let indexed: factory::DealResponse = app
        .wrap()
        .query_wasm_smart(
            factory_addr,
            &factory::QueryMsg::DealByHostEpoch {
                host: host.to_string(),
                epoch: 11,
            },
        )
        .unwrap();
    assert_eq!(indexed.address, deal_addr.to_string());
}

#[test]
fn routing_refund_transfer_failure_rolls_back_and_retry_pays_exactly_once() {
    let (mut app, native, token, _, deal_addr, host, buyer, _) = funding_fixture!(true);
    app.execute_contract(
        buyer.clone(),
        token.clone(),
        &FaultCw20ExecuteMsg::Send {
            contract: deal_addr.to_string(),
            amount: Uint128::new(FUNDING_BUDGET),
            msg: to_json_binary(&deal::Cw20HookMsg::Fund {}).unwrap(),
        },
        &[],
    )
    .unwrap();
    {
        let mut state = native.lock().unwrap();
        state.current_epoch = 11;
        state.claim_recipients.clear();
    }
    app.execute_contract(
        host.clone(),
        token.clone(),
        &FaultCw20ExecuteMsg::ConfigureFailure {
            fail_on_transfer: Some(1),
        },
        &[],
    )
    .unwrap();
    let state_before: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    let balances_before = [
        cw20_balance!(app, token, deal_addr),
        cw20_balance!(app, token, buyer),
        cw20_balance!(app, token, host),
    ];
    let error = app
        .execute_contract(
            "caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Refund {},
            &[],
        )
        .unwrap_err();
    assert!(error
        .root_cause()
        .to_string()
        .contains("deliberate CW20 transfer failure #1"));
    let state_after_failure: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    assert_eq!(state_after_failure, state_before);
    assert_eq!(
        [
            cw20_balance!(app, token, deal_addr),
            cw20_balance!(app, token, buyer),
            cw20_balance!(app, token, host),
        ],
        balances_before
    );

    app.execute_contract(
        host.clone(),
        token.clone(),
        &FaultCw20ExecuteMsg::ConfigureFailure {
            fail_on_transfer: None,
        },
        &[],
    )
    .unwrap();
    app.execute_contract(
        "retry-caller".into_addr(),
        deal_addr.clone(),
        &deal::ExecuteMsg::Refund {},
        &[],
    )
    .unwrap();
    assert_eq!(cw20_balance!(app, token, deal_addr), Uint128::zero());
    assert_eq!(
        cw20_balance!(app, token, buyer),
        Uint128::new(FUNDING_BUDGET * 3)
    );
    assert_eq!(cw20_balance!(app, token, host), Uint128::zero());
    let repeat = app
        .execute_contract(
            "repeat-caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Refund {},
            &[],
        )
        .unwrap_err();
    assert!(repeat.root_cause().to_string().contains("state Refunded"));
    assert_eq!(
        cw20_balance!(app, token, buyer),
        Uint128::new(FUNDING_BUDGET * 3)
    );
}

#[test]
fn routing_refund_epoch_and_evidence_matrix_uses_real_cw20_ledgers() {
    for case in 0..5 {
        let (mut app, native, token, _, deal_addr, _, buyer, _) = funding_fixture!();
        app.execute_contract(
            buyer.clone(),
            token.clone(),
            &Cw20ExecuteMsg::Send {
                contract: deal_addr.to_string(),
                amount: Uint128::new(FUNDING_BUDGET),
                msg: to_json_binary(&deal::Cw20HookMsg::Fund {}).unwrap(),
            },
            &[],
        )
        .unwrap();
        {
            let mut state = native.lock().unwrap();
            match case {
                0 => {
                    state.current_epoch = 10;
                    state.claim_recipients.clear();
                }
                1 => {
                    state.current_epoch = 11;
                    state.claim_recipients.clear();
                }
                2 => {
                    state.current_epoch = 15;
                    state.claim_recipients = vec![ClaimRecipientEntry {
                        epoch: 11,
                        recipient: "other-deal".into_addr().to_string(),
                    }];
                }
                3 => {
                    state.current_epoch = 16;
                    state.claim_recipients.clear();
                }
                _ => {
                    state.current_epoch = 11;
                }
            }
        }
        let result = app.execute_contract(
            "caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Refund {},
            &[],
        );
        if matches!(case, 1 | 2) {
            let response = result.unwrap();
            let expected_reason = if case == 1 {
                "routing_missing"
            } else {
                "routing_mismatch"
            };
            assert_eq!(
                event_attribute(&response, "wasm-deal_refunded", "reason"),
                Some(expected_reason)
            );
            if case == 2 {
                assert_eq!(
                    event_attribute(&response, "wasm-deal_refunded", "observed_recipient"),
                    Some("other-deal".into_addr().as_str())
                );
            }
            assert_eq!(cw20_balance!(app, token, deal_addr), Uint128::zero());
            assert_eq!(
                cw20_balance!(app, token, buyer),
                Uint128::new(FUNDING_BUDGET * 3)
            );
        } else {
            let error = result.unwrap_err();
            let expected = match case {
                0 => "window has not opened",
                3 => "routing-proof window is closed",
                _ => "still routes exactly",
            };
            assert!(error.root_cause().to_string().contains(expected));
            assert_eq!(
                cw20_balance!(app, token, deal_addr),
                Uint128::new(FUNDING_BUDGET)
            );
            assert_eq!(
                cw20_balance!(app, token, buyer),
                Uint128::new(FUNDING_BUDGET * 2)
            );
        }
    }
}

#[test]
fn routing_refund_fails_closed_for_exact_errors_and_pruning() {
    for case in 0..5 {
        let (mut app, native, token, _, deal_addr, _, buyer, _) = funding_fixture!();
        app.execute_contract(
            buyer.clone(),
            token.clone(),
            &Cw20ExecuteMsg::Send {
                contract: deal_addr.to_string(),
                amount: Uint128::new(FUNDING_BUDGET),
                msg: to_json_binary(&deal::Cw20HookMsg::Fund {}).unwrap(),
            },
            &[],
        )
        .unwrap();
        {
            let mut state = native.lock().unwrap();
            state.current_epoch = if case == 4 { 16 } else { 11 };
            match case {
                0 => {}
                1 => state.claim_recipients_response = FundingQueryResponse::QueryFailure,
                2 => state.claim_recipients_response = FundingQueryResponse::Malformed,
                3 => state.claim_recipients_response = FundingQueryResponse::Oversized,
                _ => state.claim_recipients.clear(),
            }
        }
        let state_before: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap();
        let balances_before = [
            cw20_balance!(app, token, deal_addr),
            cw20_balance!(app, token, buyer),
        ];
        let error = app
            .execute_contract(
                "caller".into_addr(),
                deal_addr.clone(),
                &deal::ExecuteMsg::Refund {},
                &[],
            )
            .unwrap_err();
        let expected = match case {
            0 => "still routes exactly",
            1 => "Gonka query failed",
            2 => "failed to decode protobuf response",
            3 => "is too large",
            _ => "routing-proof window is closed",
        };
        assert!(
            error.root_cause().to_string().contains(expected),
            "unexpected case {case}: {}",
            error.root_cause()
        );
        assert_eq!(
            app.wrap()
                .query_wasm_smart::<deal::StateResponse>(
                    deal_addr.clone(),
                    &deal::QueryMsg::State {}
                )
                .unwrap(),
            state_before
        );
        assert_eq!(
            [
                cw20_balance!(app, token, deal_addr),
                cw20_balance!(app, token, buyer),
            ],
            balances_before
        );
    }
}

#[test]
fn claim_expiry_refunds_exact_deposit_preserves_assets_index_and_host_only_release() {
    let (mut app, native, token, factory_addr, deal_addr, _, host, buyer, fees) = settlement_fixture!(
        false,
        true,
        FUNDING_BUDGET,
        1_000_000,
        "host",
        "buyer",
        "fees"
    );
    {
        let mut state = native.lock().unwrap();
        state.current_epoch = 13;
    }
    set_native_performance(&native, 11, host.to_string(), 10, 20, false);

    let donation = Uint128::new(7_000_000);
    app.execute_contract(
        buyer.clone(),
        token.clone(),
        &Cw20ExecuteMsg::Transfer {
            recipient: deal_addr.to_string(),
            amount: donation,
        },
        &[],
    )
    .unwrap();
    app.sudo(SudoMsg::Bank(BankSudo::Mint {
        to_address: deal_addr.to_string(),
        amount: vec![coin(13, "ngonka"), coin(17, "uatom")],
    }))
    .unwrap();

    let response = app
        .execute_contract(
            "permissionless-caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Refund {},
            &[],
        )
        .unwrap();
    assert_eq!(
        event_attribute(&response, "wasm-deal_refunded", "reason"),
        Some("claim_expiry")
    );
    assert_eq!(
        event_attribute(&response, "wasm-usdt_refunded", "amount_micro_usdt"),
        Some(FUNDING_BUDGET.to_string().as_str())
    );
    assert_eq!(
        cw20_balance!(app, token, buyer),
        Uint128::new(FUNDING_BUDGET * 4) - donation
    );
    assert_eq!(cw20_balance!(app, token, deal_addr), donation);
    assert_eq!(cw20_balance!(app, token, host), Uint128::zero());
    assert_eq!(cw20_balance!(app, token, fees), Uint128::zero());
    assert_eq!(
        app.wrap()
            .query_balance(&deal_addr, "ngonka")
            .unwrap()
            .amount,
        Uint128::new(13)
    );
    assert_eq!(
        app.wrap()
            .query_balance(&deal_addr, "uatom")
            .unwrap()
            .amount,
        Uint128::new(17)
    );

    let state: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    assert_eq!(state.status, deal::DealStatus::Refunded);
    assert_eq!(state.refund_reason, Some(deal::RefundReason::ClaimExpiry));
    assert_eq!(state.gnk_release_policy, deal::GnkReleasePolicy::HostOnly);
    assert_eq!(state.buyer_refund_usdt, Uint128::new(FUNDING_BUDGET));
    assert_eq!(state.gross_usdt, Uint128::zero());
    assert_eq!(state.fee_usdt, Uint128::zero());
    assert_eq!(state.host_net_usdt, Uint128::zero());
    assert_eq!(state.total_claim_ngonka, Uint128::zero());

    app.sudo(SudoMsg::Bank(BankSudo::Mint {
        to_address: deal_addr.to_string(),
        amount: vec![coin(2, "ngonka")],
    }))
    .unwrap();

    for msg in [
        deal::ExecuteMsg::Refund {},
        deal::ExecuteMsg::SettleClaim {},
    ] {
        let error = app
            .execute_contract("repeat".into_addr(), deal_addr.clone(), &msg, &[])
            .unwrap_err();
        assert!(error.root_cause().to_string().contains("state Refunded"));
    }

    app.execute_contract(
        "release-caller".into_addr(),
        deal_addr.clone(),
        &deal::ExecuteMsg::ReleaseUnlockedGnk {},
        &[],
    )
    .unwrap();
    assert_eq!(
        app.wrap().query_balance(&host, "ngonka").unwrap().amount,
        Uint128::new(15)
    );
    assert_eq!(
        app.wrap()
            .query_balance(&deal_addr, "uatom")
            .unwrap()
            .amount,
        Uint128::new(17)
    );
    assert_eq!(cw20_balance!(app, token, deal_addr), donation);

    let indexed: factory::DealResponse = app
        .wrap()
        .query_wasm_smart(
            factory_addr,
            &factory::QueryMsg::DealByHostEpoch {
                host: host.to_string(),
                epoch: 11,
            },
        )
        .unwrap();
    assert_eq!(indexed.address, deal_addr.to_string());
}

#[test]
fn no_sale_claim_expiry_emits_no_transfer_and_keeps_host_only_lifecycle() {
    for (earned, rewarded) in [(0, 0), (10, 20)] {
        let (mut app, native, token, factory_addr, deal_addr, _, host, buyer, fees) = settlement_fixture!(
            false,
            false,
            FUNDING_BUDGET,
            1_000_000,
            "host",
            "buyer",
            "fees"
        );
        native.lock().unwrap().current_epoch = 13;
        set_native_performance(&native, 11, host.to_string(), earned, rewarded, false);
        app.sudo(SudoMsg::Bank(BankSudo::Mint {
            to_address: deal_addr.to_string(),
            amount: vec![coin(5, "ngonka"), coin(9, "uatom")],
        }))
        .unwrap();

        let response = app
            .execute_contract(
                "permissionless-caller".into_addr(),
                deal_addr.clone(),
                &deal::ExecuteMsg::Refund {},
                &[],
            )
            .unwrap();
        assert_eq!(
            event_attribute(&response, "wasm-deal_expired", "reason"),
            Some("claim_expiry")
        );
        assert!(response
            .events
            .iter()
            .all(|event| event.ty != "wasm-usdt_refunded" && event.ty != "wasm-usdt_paid"));
        assert_eq!(cw20_balance!(app, token, deal_addr), Uint128::zero());
        assert_eq!(
            cw20_balance!(app, token, buyer),
            Uint128::new(FUNDING_BUDGET * 4)
        );
        assert_eq!(cw20_balance!(app, token, host), Uint128::zero());
        assert_eq!(cw20_balance!(app, token, fees), Uint128::zero());

        let state: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap();
        assert_eq!(state.status, deal::DealStatus::Expired);
        assert_eq!(state.refund_reason, Some(deal::RefundReason::ClaimExpiry));
        assert_eq!(state.gnk_release_policy, deal::GnkReleasePolicy::HostOnly);
        assert_eq!(state.buyer_refund_usdt, Uint128::zero());

        app.sudo(SudoMsg::Bank(BankSudo::Mint {
            to_address: deal_addr.to_string(),
            amount: vec![coin(3, "ngonka")],
        }))
        .unwrap();

        app.execute_contract(
            "release-caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::ReleaseUnlockedGnk {},
            &[],
        )
        .unwrap();
        assert_eq!(
            app.wrap().query_balance(&host, "ngonka").unwrap().amount,
            Uint128::new(8)
        );
        assert_eq!(
            app.wrap()
                .query_balance(&deal_addr, "uatom")
                .unwrap()
                .amount,
            Uint128::new(9)
        );
        let indexed: factory::DealResponse = app
            .wrap()
            .query_wasm_smart(
                factory_addr,
                &factory::QueryMsg::DealByHostEpoch {
                    host: host.to_string(),
                    epoch: 11,
                },
            )
            .unwrap();
        assert_eq!(indexed.address, deal_addr.to_string());
    }
}

#[test]
fn claim_expiry_fails_closed_at_e_plus_one_for_claimed_and_invalid_evidence() {
    for case in 0..8 {
        let (mut app, native, token, _, deal_addr, _, host, buyer, _) = settlement_fixture!(
            false,
            true,
            FUNDING_BUDGET,
            1_000_000,
            "host",
            "buyer",
            "fees"
        );
        {
            let mut state = native.lock().unwrap();
            state.current_epoch = if case == 0 { 12 } else { 13 };
            match case {
                1 | 2 => {
                    state.performance_summary = Some(EpochPerformanceSummary {
                        epoch_index: 11,
                        participant_id: host.to_string(),
                        earned_coins: if case == 2 { 10 } else { 0 },
                        rewarded_coins: if case == 2 { 20 } else { 0 },
                        claimed: true,
                        ..EpochPerformanceSummary::default()
                    });
                }
                3 => state.performance_summary = None,
                4 => state.performance_response = FundingQueryResponse::QueryFailure,
                5 => state.performance_response = FundingQueryResponse::Malformed,
                6 => state.performance_response = FundingQueryResponse::Oversized,
                _ => {
                    state.performance_summary = Some(EpochPerformanceSummary {
                        epoch_index: 12,
                        participant_id: host.to_string(),
                        claimed: false,
                        ..EpochPerformanceSummary::default()
                    });
                }
            }
        }
        let state_before: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap();
        let balances_before = [
            cw20_balance!(app, token, deal_addr),
            cw20_balance!(app, token, buyer),
            cw20_balance!(app, token, host),
        ];
        let error = app
            .execute_contract(
                "caller".into_addr(),
                deal_addr.clone(),
                &deal::ExecuteMsg::Refund {},
                &[],
            )
            .unwrap_err();
        let expected = match case {
            0 => "first allowed epoch 13",
            1 | 2 => "already confirmed",
            3 => "missing required field",
            4 => "Gonka query failed",
            5 => "failed to decode protobuf response",
            6 => "is too large",
            _ => "mismatched epoch_index",
        };
        assert!(
            error.root_cause().to_string().contains(expected),
            "unexpected case {case}: {}",
            error.root_cause()
        );
        assert_eq!(
            app.wrap()
                .query_wasm_smart::<deal::StateResponse>(
                    deal_addr.clone(),
                    &deal::QueryMsg::State {}
                )
                .unwrap(),
            state_before
        );
        assert_eq!(
            [
                cw20_balance!(app, token, deal_addr),
                cw20_balance!(app, token, buyer),
                cw20_balance!(app, token, host),
            ],
            balances_before
        );
    }
}

#[test]
fn c_network_unconfirmed_refund_rolls_back_retries_and_keeps_terminal_host_only_economics() {
    let (mut app, native, token, factory_addr, deal_addr, controller, host, buyer, fees) = settlement_fixture!(
        true,
        true,
        FUNDING_BUDGET,
        1_000_000,
        "host",
        "buyer",
        "fees"
    );
    {
        let mut state = native.lock().unwrap();
        // The native claim may really have happened; the query failure hides
        // that fact from the contract at the moment Refund executes.
        state.performance_summary = Some(EpochPerformanceSummary {
            epoch_index: 11,
            participant_id: host.to_string(),
            earned_coins: 10,
            rewarded_coins: 20,
            claimed: true,
            ..EpochPerformanceSummary::default()
        });
        state.performance_response = FundingQueryResponse::QueryFailure;
    }

    // The same hidden claimed summary must not release a deposit at E+2.
    native.lock().unwrap().current_epoch = 13;
    assert!(
        native
            .lock()
            .unwrap()
            .performance_summary
            .as_ref()
            .unwrap()
            .claimed
    );
    println!("C_CASE:r5-cancel-native"); // modeled claim only, not native ledger proof
    assert_hidden_claim_preserves_state_and_balances!(app, token, deal_addr, buyer, host, fees);
    println!("C_CASE:r5-cancel-probe");
    println!("C_CASE:r5-cancel-e2");
    native.lock().unwrap().current_epoch = 14;

    let donation = Uint128::new(7_000_000);
    app.execute_contract(
        buyer.clone(),
        token.clone(),
        &Cw20ExecuteMsg::Transfer {
            recipient: deal_addr.to_string(),
            amount: donation,
        },
        &[],
    )
    .unwrap();
    app.sudo(SudoMsg::Bank(BankSudo::Mint {
        to_address: deal_addr.to_string(),
        amount: vec![coin(13, "ngonka"), coin(17, "uatom")],
    }))
    .unwrap();

    app.execute_contract(
        controller.clone(),
        token.clone(),
        &FaultCw20ExecuteMsg::ConfigureFailure {
            fail_on_transfer: Some(1),
        },
        &[],
    )
    .unwrap();
    let state_before: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    let balances_before = [
        cw20_balance!(app, token, deal_addr),
        cw20_balance!(app, token, buyer),
    ];
    let error = app
        .execute_contract(
            "caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Refund {},
            &[],
        )
        .unwrap_err();
    assert!(error
        .root_cause()
        .to_string()
        .contains("deliberate CW20 transfer failure #1"));
    assert_eq!(
        app.wrap()
            .query_wasm_smart::<deal::StateResponse>(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap(),
        state_before
    );
    assert_eq!(
        [
            cw20_balance!(app, token, deal_addr),
            cw20_balance!(app, token, buyer),
        ],
        balances_before
    );

    app.execute_contract(
        controller,
        token.clone(),
        &FaultCw20ExecuteMsg::ConfigureFailure {
            fail_on_transfer: None,
        },
        &[],
    )
    .unwrap();
    let response = app
        .execute_contract(
            "retry".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Refund {},
            &[],
        )
        .unwrap();
    assert_eq!(
        event_attribute(&response, "wasm-deal_refunded", "reason"),
        Some("network_unconfirmed")
    );
    let refunded: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    assert_eq!(refunded.status, deal::DealStatus::Refunded);
    assert_eq!(
        refunded.refund_reason,
        Some(deal::RefundReason::NetworkUnconfirmed)
    );
    assert_eq!(
        refunded.gnk_release_policy,
        deal::GnkReleasePolicy::HostOnly
    );
    assert_eq!(refunded.buyer_refund_usdt, Uint128::new(FUNDING_BUDGET));
    assert_eq!(refunded.host_net_usdt, Uint128::zero());
    assert_eq!(cw20_balance!(app, token, deal_addr), donation);
    assert_eq!(
        cw20_balance!(app, token, buyer),
        Uint128::new(FUNDING_BUDGET * 4) - donation
    );
    assert_eq!(cw20_balance!(app, token, host), Uint128::zero());
    assert_eq!(cw20_balance!(app, token, fees), Uint128::zero());

    // Recovery reveals claimed=true, but the terminal emergency outcome cannot
    // be settled or refunded a second time.
    native.lock().unwrap().performance_response = FundingQueryResponse::Valid;
    for msg in [
        deal::ExecuteMsg::SettleClaim {},
        deal::ExecuteMsg::Refund {},
    ] {
        let repeat = app
            .execute_contract("repeat".into_addr(), deal_addr.clone(), &msg, &[])
            .unwrap_err();
        assert!(repeat.root_cause().to_string().contains("state Refunded"));
    }

    app.sudo(SudoMsg::Bank(BankSudo::Mint {
        to_address: deal_addr.to_string(),
        amount: vec![coin(2, "ngonka")],
    }))
    .unwrap();
    app.execute_contract(
        "release".into_addr(),
        deal_addr.clone(),
        &deal::ExecuteMsg::ReleaseUnlockedGnk {},
        &[],
    )
    .unwrap();
    assert_eq!(
        app.wrap().query_balance(&host, "ngonka").unwrap().amount,
        Uint128::new(15)
    );
    assert_eq!(
        app.wrap().query_balance(&buyer, "ngonka").unwrap().amount,
        Uint128::zero()
    );
    assert_eq!(
        app.wrap()
            .query_balance(&deal_addr, "uatom")
            .unwrap()
            .amount,
        Uint128::new(17)
    );
    assert_eq!(cw20_balance!(app, token, deal_addr), donation);

    let indexed: factory::DealResponse = app
        .wrap()
        .query_wasm_smart(
            factory_addr,
            &factory::QueryMsg::DealByHostEpoch {
                host: host.to_string(),
                epoch: 11,
            },
        )
        .unwrap();
    assert_eq!(indexed.address, deal_addr.to_string());
    for case in [
        "r6.3",
        "r5-cancel-e3",
        "r5-cancel-ledger",
        "r5-cancel-terminal-refund",
        "r5-cancel-terminal-settle_claim",
    ] {
        println!("C_CASE:{case}");
    }
}

#[test]
fn network_unconfirmed_no_sale_expires_without_transfer() {
    let (mut app, native, token, _, deal_addr, _, host, buyer, fees) = settlement_fixture!(
        false,
        false,
        FUNDING_BUDGET,
        1_000_000,
        "host",
        "buyer",
        "fees"
    );
    {
        let mut state = native.lock().unwrap();
        state.current_epoch = 14;
        state.performance_response = FundingQueryResponse::QueryFailure;
    }
    let response = app
        .execute_contract(
            "caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Refund {},
            &[],
        )
        .unwrap();
    assert_eq!(
        event_attribute(&response, "wasm-deal_expired", "reason"),
        Some("network_unconfirmed")
    );
    assert!(response
        .events
        .iter()
        .all(|event| event.ty != "wasm-usdt_refunded" && event.ty != "wasm-usdt_paid"));
    let expired: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr, &deal::QueryMsg::State {})
        .unwrap();
    assert_eq!(expired.status, deal::DealStatus::Expired);
    assert_eq!(
        expired.refund_reason,
        Some(deal::RefundReason::NetworkUnconfirmed)
    );
    assert_eq!(
        cw20_balance!(app, token, buyer),
        Uint128::new(FUNDING_BUDGET * 4)
    );
    assert_eq!(cw20_balance!(app, token, host), Uint128::zero());
    assert_eq!(cw20_balance!(app, token, fees), Uint128::zero());
}

#[test]
fn claim_expiry_transfer_failure_rolls_back_and_retry_pays_once() {
    let (mut app, native, token, _, deal_addr, controller, host, buyer, _) = settlement_fixture!(
        true,
        true,
        FUNDING_BUDGET,
        1_000_000,
        "host",
        "buyer",
        "fees"
    );
    native.lock().unwrap().current_epoch = 13;
    set_native_performance(&native, 11, host.to_string(), 0, 0, false);
    app.execute_contract(
        controller.clone(),
        token.clone(),
        &FaultCw20ExecuteMsg::ConfigureFailure {
            fail_on_transfer: Some(1),
        },
        &[],
    )
    .unwrap();
    let state_before: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    let balances_before = [
        cw20_balance!(app, token, deal_addr),
        cw20_balance!(app, token, buyer),
    ];
    let error = app
        .execute_contract(
            "caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Refund {},
            &[],
        )
        .unwrap_err();
    assert!(error
        .root_cause()
        .to_string()
        .contains("deliberate CW20 transfer failure #1"));
    assert_eq!(
        app.wrap()
            .query_wasm_smart::<deal::StateResponse>(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap(),
        state_before
    );
    assert_eq!(
        [
            cw20_balance!(app, token, deal_addr),
            cw20_balance!(app, token, buyer),
        ],
        balances_before
    );

    app.execute_contract(
        controller,
        token.clone(),
        &FaultCw20ExecuteMsg::ConfigureFailure {
            fail_on_transfer: None,
        },
        &[],
    )
    .unwrap();
    app.execute_contract(
        "retry".into_addr(),
        deal_addr.clone(),
        &deal::ExecuteMsg::Refund {},
        &[],
    )
    .unwrap();
    assert_eq!(cw20_balance!(app, token, deal_addr), Uint128::zero());
    assert_eq!(
        cw20_balance!(app, token, buyer),
        Uint128::new(FUNDING_BUDGET * 4)
    );
    let repeat = app
        .execute_contract(
            "repeat".into_addr(),
            deal_addr,
            &deal::ExecuteMsg::Refund {},
            &[],
        )
        .unwrap_err();
    assert!(repeat.root_cause().to_string().contains("state Refunded"));
}

#[test]
fn failed_lock_and_cancel_proofs_preserve_real_deal_state_and_balances() {
    let (mut app, native, token, _, deal_addr, _, buyer, _) = funding_fixture!();
    native.lock().unwrap().current_epoch = 11;
    let state_before: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    let buyer_before: BalanceResponse = app
        .wrap()
        .query_wasm_smart(
            token.clone(),
            &Cw20QueryMsg::Balance {
                address: buyer.to_string(),
            },
        )
        .unwrap();
    let deal_before: BalanceResponse = app
        .wrap()
        .query_wasm_smart(
            token.clone(),
            &Cw20QueryMsg::Balance {
                address: deal_addr.to_string(),
            },
        )
        .unwrap();

    native.lock().unwrap().claim_recipients.clear();
    let missing_lock = app
        .execute_contract(
            "caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Lock {},
            &[],
        )
        .unwrap_err();
    assert!(missing_lock
        .root_cause()
        .to_string()
        .contains("claim recipient is not configured for epoch 11"));

    native.lock().unwrap().claim_recipients = vec![ClaimRecipientEntry {
        epoch: 11,
        recipient: deal_addr.to_string(),
    }];
    let exact_cancel = app
        .execute_contract(
            "caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Cancel {},
            &[],
        )
        .unwrap_err();
    assert!(exact_cancel
        .root_cause()
        .to_string()
        .contains("still routes exactly to this Deal"));

    {
        let mut state = native.lock().unwrap();
        state.claim_recipients.clear();
        state.claim_recipients_response = FundingQueryResponse::QueryFailure;
    }
    let query_failure = app
        .execute_contract(
            "caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Cancel {},
            &[],
        )
        .unwrap_err();
    assert!(query_failure
        .root_cause()
        .to_string()
        .contains("Gonka query failed"));

    {
        let mut state = native.lock().unwrap();
        state.current_epoch = 16;
        state.claim_recipients_response = FundingQueryResponse::Valid;
    }
    let pruned_window = app
        .execute_contract(
            "caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Cancel {},
            &[],
        )
        .unwrap_err();
    assert!(pruned_window
        .root_cause()
        .to_string()
        .contains("permissionless cancel window is closed"));

    let state_after: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    let buyer_after: BalanceResponse = app
        .wrap()
        .query_wasm_smart(
            token.clone(),
            &Cw20QueryMsg::Balance {
                address: buyer.to_string(),
            },
        )
        .unwrap();
    let deal_after: BalanceResponse = app
        .wrap()
        .query_wasm_smart(
            token,
            &Cw20QueryMsg::Balance {
                address: deal_addr.to_string(),
            },
        )
        .unwrap();
    assert_eq!(state_after, state_before);
    assert_eq!(buyer_after, buyer_before);
    assert_eq!(deal_after, deal_before);
}

#[derive(Clone)]
struct RecordingAddressGenerator {
    generated: Arc<Mutex<Vec<Addr>>>,
}

impl AddressGenerator for RecordingAddressGenerator {
    fn contract_address(
        &self,
        api: &dyn Api,
        storage: &mut dyn Storage,
        code_id: u64,
        instance_id: u64,
    ) -> AnyResult<Addr> {
        let address =
            SimpleAddressGenerator.contract_address(api, storage, code_id, instance_id)?;
        self.generated.lock().unwrap().push(address.clone());
        Ok(address)
    }
}

#[test]
fn contracts_store_config_and_no_admin_blocks_code_replacement() {
    let mut app = AppBuilder::new()
        .with_stargate(EpochStargate)
        .build(|_, _, _| {});
    let owner = "owner".into_addr();

    let cw20_code_id = app.store_code(cw20_contract());
    let factory_code_id = app.store_code(factory_contract());
    let deal_code_id = app.store_code(deal_contract());
    let migration_target_code_id = app.store_code(migration_target_contract());
    let token = app
        .instantiate_contract(
            cw20_code_id,
            owner.clone(),
            &cw20_base::msg::InstantiateMsg {
                name: "Test USDT".to_string(),
                symbol: "USDT".to_string(),
                decimals: 6,
                initial_balances: vec![],
                mint: None,
                marketing: None,
            },
            &[],
            "test-usdt",
            None,
        )
        .unwrap();
    let factory_addr = app
        .instantiate_contract(
            factory_code_id,
            owner.clone(),
            &factory::InstantiateMsg {
                deal_code_id,
                settlement_cw20: token.to_string(),
                fee_recipient: "fees".into_addr().to_string(),
                fee_bps: 150,
            },
            &[],
            "marketplace-factory",
            None,
        )
        .unwrap();
    let host = "host".into_addr();
    let create_response = app
        .execute_contract(
            host.clone(),
            factory_addr.clone(),
            &factory::ExecuteMsg::CreateOffer {
                target_epoch: 11,
                price_micro_usdt_per_gnk: Uint128::new(1_000_000),
                buyer_budget_micro_usdt: Uint128::new(100_000_000),
            },
            &[],
        )
        .unwrap();
    let by_id: factory::DealResponse = app
        .wrap()
        .query_wasm_smart(factory_addr.clone(), &factory::QueryMsg::Deal { id: 1 })
        .unwrap();
    let by_host_epoch: factory::DealResponse = app
        .wrap()
        .query_wasm_smart(
            factory_addr.clone(),
            &factory::QueryMsg::DealByHostEpoch {
                host: host.to_string(),
                epoch: 11,
            },
        )
        .unwrap();
    assert_eq!(by_id.address, by_host_epoch.address);
    let deal_addr = Addr::unchecked(by_id.address);
    let offer_event = create_response
        .events
        .iter()
        .find(|event| event.ty == "wasm-offer_created")
        .unwrap();
    assert!(offer_event
        .attributes
        .iter()
        .any(|attribute| attribute.key == "deal_id" && attribute.value == "1"));

    let factory_config: factory::ConfigResponse = app
        .wrap()
        .query_wasm_smart(factory_addr.clone(), &factory::QueryMsg::Config {})
        .unwrap();
    assert_eq!(factory_config.deal_code_id, deal_code_id);
    let deal_state: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
        .unwrap();
    assert_eq!(deal_state.status, deal::DealStatus::Open);
    let deal_config: deal::ConfigResponse = app
        .wrap()
        .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::Config {})
        .unwrap();
    assert_eq!(deal_config.factory, factory_addr.to_string());
    assert_eq!(deal_config.host, host.to_string());
    assert_eq!(deal_config.target_epoch, 11);
    assert_eq!(
        deal_config.price_micro_usdt_per_gnk,
        Uint128::new(1_000_000)
    );
    assert_eq!(
        deal_config.buyer_budget_micro_usdt,
        Uint128::new(100_000_000)
    );
    assert_eq!(deal_config.settlement_cw20, token.to_string());
    assert_eq!(deal_config.fee_recipient, "fees".into_addr().to_string());
    assert_eq!(deal_config.fee_bps, 150);
    assert_eq!(deal_config.pinned_gonka_sha, gonka_proto::SOURCE_COMMIT);

    let factory_info = app
        .wrap()
        .query_wasm_contract_info(factory_addr.clone())
        .unwrap();
    let deal_info = app
        .wrap()
        .query_wasm_contract_info(deal_addr.clone())
        .unwrap();
    assert_eq!(factory_info.admin, None);
    assert_eq!(deal_info.admin, None);

    // The replacement code deliberately has a migrate entry point. The
    // protection comes from chain-level admin=None, not from the old code
    // omitting its own migrate entry point.
    for (address, original_code_id) in [(factory_addr, factory_code_id), (deal_addr, deal_code_id)]
    {
        let error = app
            .migrate_contract(
                owner.clone(),
                address.clone(),
                &Empty {},
                migration_target_code_id,
            )
            .unwrap_err();
        assert!(error
            .root_cause()
            .to_string()
            .contains("Only admin can migrate contract"));
        assert_eq!(
            app.wrap()
                .query_wasm_contract_info(address)
                .unwrap()
                .code_id,
            original_code_id
        );
    }
}

#[test]
fn multiple_host_epoch_lifecycles_keep_ledgers_recipients_and_counters_isolated() {
    const DEAL_A1_BUDGET: u128 = 80_000_000;
    const DEAL_B1_BUDGET: u128 = 30_000_000;
    const DEAL_A2_BUDGET: u128 = 50_000_000;

    let owner = "owner".into_addr();
    let host_a = "host-a".into_addr();
    let host_b = "host-b".into_addr();
    let buyer_a = "buyer-a".into_addr();
    let buyer_b = "buyer-b".into_addr();
    let buyer_c = "buyer-c".into_addr();
    let fee_recipient = "fees".into_addr();
    let other_token_donor = "other-token-donor".into_addr();
    let native_state = Arc::new(Mutex::new(MultiDealNativeState {
        current_epoch: 10,
        claim_recipients: BTreeMap::new(),
        performance_summaries: BTreeMap::new(),
    }));
    let mut app = AppBuilder::new()
        .with_stargate(MultiDealStargate {
            state: native_state.clone(),
        })
        .build(|_, _, _| {});
    let cw20_code_id = app.store_code(cw20_contract());
    let factory_code_id = app.store_code(factory_contract());
    let deal_code_id = app.store_code(deal_contract());
    let token = app
        .instantiate_contract(
            cw20_code_id,
            owner.clone(),
            &cw20_base::msg::InstantiateMsg {
                name: "Test USDT".to_string(),
                symbol: "USDT".to_string(),
                decimals: 6,
                initial_balances: vec![
                    Cw20Coin {
                        address: buyer_a.to_string(),
                        amount: Uint128::new(DEAL_A1_BUDGET),
                    },
                    Cw20Coin {
                        address: buyer_b.to_string(),
                        amount: Uint128::new(DEAL_B1_BUDGET),
                    },
                    Cw20Coin {
                        address: buyer_c.to_string(),
                        amount: Uint128::new(DEAL_A2_BUDGET),
                    },
                ],
                mint: None,
                marketing: None,
            },
            &[],
            "test-usdt",
            None,
        )
        .unwrap();
    let other_token = app
        .instantiate_contract(
            cw20_code_id,
            owner.clone(),
            &cw20_base::msg::InstantiateMsg {
                name: "Unrelated Token".to_string(),
                symbol: "OTHER".to_string(),
                decimals: 6,
                initial_balances: vec![Cw20Coin {
                    address: other_token_donor.to_string(),
                    amount: Uint128::new(777),
                }],
                mint: None,
                marketing: None,
            },
            &[],
            "unrelated-token",
            None,
        )
        .unwrap();
    let factory_addr = app
        .instantiate_contract(
            factory_code_id,
            owner,
            &factory::InstantiateMsg {
                deal_code_id,
                settlement_cw20: token.to_string(),
                fee_recipient: fee_recipient.to_string(),
                fee_bps: 150,
            },
            &[],
            "marketplace-factory",
            None,
        )
        .unwrap();

    for (host, epoch, price, budget) in [
        (host_a.clone(), 11, 1_000_000, DEAL_A1_BUDGET),
        (host_b.clone(), 11, 2_000_000, DEAL_B1_BUDGET),
        (host_a.clone(), 12, 1_000_000, DEAL_A2_BUDGET),
    ] {
        app.execute_contract(
            host,
            factory_addr.clone(),
            &factory::ExecuteMsg::CreateOffer {
                target_epoch: epoch,
                price_micro_usdt_per_gnk: Uint128::new(price),
                buyer_budget_micro_usdt: Uint128::new(budget),
            },
            &[],
        )
        .unwrap();
    }
    let deal_a1 = Addr::unchecked(
        app.wrap()
            .query_wasm_smart::<factory::DealResponse>(
                factory_addr.clone(),
                &factory::QueryMsg::Deal { id: 1 },
            )
            .unwrap()
            .address,
    );
    let deal_b1 = Addr::unchecked(
        app.wrap()
            .query_wasm_smart::<factory::DealResponse>(
                factory_addr.clone(),
                &factory::QueryMsg::Deal { id: 2 },
            )
            .unwrap()
            .address,
    );
    let deal_a2 = Addr::unchecked(
        app.wrap()
            .query_wasm_smart::<factory::DealResponse>(
                factory_addr.clone(),
                &factory::QueryMsg::Deal { id: 3 },
            )
            .unwrap()
            .address,
    );
    assert_ne!(deal_a1, deal_b1);
    assert_ne!(deal_a1, deal_a2);
    assert_ne!(deal_b1, deal_a2);
    {
        let mut native = native_state.lock().unwrap();
        native.claim_recipients.insert(
            host_a.to_string(),
            vec![
                ClaimRecipientEntry {
                    epoch: 11,
                    recipient: deal_a1.to_string(),
                },
                ClaimRecipientEntry {
                    epoch: 12,
                    recipient: deal_a2.to_string(),
                },
            ],
        );
        native.claim_recipients.insert(
            host_b.to_string(),
            vec![ClaimRecipientEntry {
                epoch: 11,
                recipient: deal_b1.to_string(),
            }],
        );
    }

    for (buyer, deal_addr, budget) in [
        (buyer_a.clone(), deal_a1.clone(), DEAL_A1_BUDGET),
        (buyer_b.clone(), deal_b1.clone(), DEAL_B1_BUDGET),
        (buyer_c.clone(), deal_a2.clone(), DEAL_A2_BUDGET),
    ] {
        app.execute_contract(
            buyer,
            token.clone(),
            &Cw20ExecuteMsg::Send {
                contract: deal_addr.to_string(),
                amount: Uint128::new(budget),
                msg: to_json_binary(&deal::Cw20HookMsg::Fund {}).unwrap(),
            },
            &[],
        )
        .unwrap();
    }
    app.execute_contract(
        other_token_donor,
        other_token.clone(),
        &Cw20ExecuteMsg::Transfer {
            recipient: deal_a1.to_string(),
            amount: Uint128::new(77),
        },
        &[],
    )
    .unwrap();
    assert_eq!(
        cw20_balance!(app, token, deal_a1),
        Uint128::new(DEAL_A1_BUDGET)
    );
    assert_eq!(
        cw20_balance!(app, token, deal_b1),
        Uint128::new(DEAL_B1_BUDGET)
    );
    assert_eq!(
        cw20_balance!(app, token, deal_a2),
        Uint128::new(DEAL_A2_BUDGET)
    );

    native_state.lock().unwrap().current_epoch = 11;
    for deal_addr in [&deal_a1, &deal_b1] {
        app.execute_contract(
            "lock-keeper".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::Lock {},
            &[],
        )
        .unwrap();
    }
    native_state.lock().unwrap().current_epoch = 12;
    app.execute_contract(
        "lock-keeper".into_addr(),
        deal_a2.clone(),
        &deal::ExecuteMsg::Lock {},
        &[],
    )
    .unwrap();
    {
        let mut native = native_state.lock().unwrap();
        for (host, earned) in [
            (host_a.clone(), 100_000_000_000_u64),
            (host_b.clone(), 10_000_000_000_u64),
        ] {
            native.performance_summaries.insert(
                (host.to_string(), 11),
                EpochPerformanceSummary {
                    epoch_index: 11,
                    participant_id: host.to_string(),
                    earned_coins: earned,
                    claimed: true,
                    ..EpochPerformanceSummary::default()
                },
            );
        }
    }
    for deal_addr in [&deal_a1, &deal_b1] {
        app.execute_contract(
            "settlement-keeper".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::SettleClaim {},
            &[],
        )
        .unwrap();
    }
    {
        let mut native = native_state.lock().unwrap();
        native.current_epoch = 13;
        native.performance_summaries.insert(
            (host_a.to_string(), 12),
            EpochPerformanceSummary {
                epoch_index: 12,
                participant_id: host_a.to_string(),
                earned_coins: 25_000_000_000,
                claimed: true,
                ..EpochPerformanceSummary::default()
            },
        );
    }
    app.execute_contract(
        "settlement-keeper".into_addr(),
        deal_a2.clone(),
        &deal::ExecuteMsg::SettleClaim {},
        &[],
    )
    .unwrap();

    withdraw_all_usdt!(app, deal_a1);
    withdraw_all_usdt!(app, deal_b1);
    withdraw_all_usdt!(app, deal_a2);
    assert_eq!(cw20_balance!(app, token, deal_a1), Uint128::zero());
    assert_eq!(cw20_balance!(app, token, deal_b1), Uint128::zero());
    assert_eq!(cw20_balance!(app, token, deal_a2), Uint128::zero());
    assert_eq!(cw20_balance!(app, token, buyer_a), Uint128::zero());
    assert_eq!(cw20_balance!(app, token, buyer_b), Uint128::new(10_000_000));
    assert_eq!(cw20_balance!(app, token, buyer_c), Uint128::new(25_000_000));
    assert_eq!(cw20_balance!(app, token, host_a), Uint128::new(103_425_000));
    assert_eq!(cw20_balance!(app, token, host_b), Uint128::new(19_700_000));
    assert_eq!(
        cw20_balance!(app, token, fee_recipient),
        Uint128::new(1_875_000)
    );
    assert_eq!(cw20_balance!(app, other_token, deal_a1), Uint128::new(77));

    for (deal_addr, available, marker) in [
        (deal_a1.clone(), 50_000_000_000_u128, 101_u128),
        (deal_b1.clone(), 4_000_000_000_u128, 102_u128),
        (deal_a2.clone(), 10_000_000_000_u128, 103_u128),
    ] {
        app.sudo(SudoMsg::Bank(BankSudo::Mint {
            to_address: deal_addr.to_string(),
            amount: vec![coin(available, "ngonka"), coin(marker, "unrelated")],
        }))
        .unwrap();
        app.execute_contract(
            "release-keeper".into_addr(),
            deal_addr,
            &deal::ExecuteMsg::ReleaseUnlockedGnk {},
            &[],
        )
        .unwrap();
    }
    let partial_a1: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_a1.clone(), &deal::QueryMsg::State {})
        .unwrap();
    let partial_b1: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_b1.clone(), &deal::QueryMsg::State {})
        .unwrap();
    let partial_a2: deal::StateResponse = app
        .wrap()
        .query_wasm_smart(deal_a2.clone(), &deal::QueryMsg::State {})
        .unwrap();
    assert_eq!(
        partial_a1.released_total_ngonka,
        Uint256::from(50_000_000_000_u128)
    );
    assert_eq!(
        partial_a1.buyer_released_ngonka,
        Uint256::from(40_000_000_000_u128)
    );
    assert_eq!(
        partial_a1.host_released_ngonka,
        Uint256::from(10_000_000_000_u128)
    );
    assert_eq!(
        partial_b1.released_total_ngonka,
        Uint256::from(4_000_000_000_u128)
    );
    assert_eq!(
        partial_b1.buyer_released_ngonka,
        Uint256::from(4_000_000_000_u128)
    );
    assert_eq!(partial_b1.host_released_ngonka, Uint256::zero());
    assert_eq!(
        partial_a2.released_total_ngonka,
        Uint256::from(10_000_000_000_u128)
    );
    assert_eq!(
        partial_a2.buyer_released_ngonka,
        Uint256::from(10_000_000_000_u128)
    );
    assert_eq!(partial_a2.host_released_ngonka, Uint256::zero());

    for (deal_addr, remaining) in [
        (deal_a1.clone(), 50_000_000_000_u128),
        (deal_b1.clone(), 6_000_000_000_u128),
        (deal_a2.clone(), 15_000_000_000_u128),
    ] {
        app.sudo(SudoMsg::Bank(BankSudo::Mint {
            to_address: deal_addr.to_string(),
            amount: vec![coin(remaining, "ngonka")],
        }))
        .unwrap();
        app.execute_contract(
            "release-keeper".into_addr(),
            deal_addr,
            &deal::ExecuteMsg::ReleaseUnlockedGnk {},
            &[],
        )
        .unwrap();
    }
    assert_eq!(
        app.wrap().query_balance(&buyer_a, "ngonka").unwrap().amount,
        Uint128::new(80_000_000_000)
    );
    assert_eq!(
        app.wrap().query_balance(&buyer_b, "ngonka").unwrap().amount,
        Uint128::new(10_000_000_000)
    );
    assert_eq!(
        app.wrap().query_balance(&buyer_c, "ngonka").unwrap().amount,
        Uint128::new(25_000_000_000)
    );
    assert_eq!(
        app.wrap().query_balance(&host_a, "ngonka").unwrap().amount,
        Uint128::new(20_000_000_000)
    );
    assert_eq!(
        app.wrap().query_balance(&host_b, "ngonka").unwrap().amount,
        Uint128::zero()
    );

    for (id, host, epoch, deal_addr, total, buyer_paid, host_paid, marker) in [
        (
            1,
            host_a.clone(),
            11,
            deal_a1.clone(),
            100_000_000_000_u128,
            80_000_000_000_u128,
            20_000_000_000_u128,
            101_u128,
        ),
        (
            2,
            host_b,
            11,
            deal_b1.clone(),
            10_000_000_000_u128,
            10_000_000_000_u128,
            0_u128,
            102_u128,
        ),
        (
            3,
            host_a,
            12,
            deal_a2.clone(),
            25_000_000_000_u128,
            25_000_000_000_u128,
            0_u128,
            103_u128,
        ),
    ] {
        let state: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(deal_addr.clone(), &deal::QueryMsg::State {})
            .unwrap();
        assert_eq!(state.status, deal::DealStatus::Completed);
        assert_eq!(state.total_claim_ngonka, Uint128::new(total));
        assert_eq!(state.released_total_ngonka, Uint256::from(total));
        assert_eq!(state.buyer_released_ngonka, Uint256::from(buyer_paid));
        assert_eq!(state.host_released_ngonka, Uint256::from(host_paid));
        assert_eq!(
            app.wrap()
                .query_balance(&deal_addr, "unrelated")
                .unwrap()
                .amount,
            Uint128::new(marker)
        );
        let by_id: factory::DealResponse = app
            .wrap()
            .query_wasm_smart(factory_addr.clone(), &factory::QueryMsg::Deal { id })
            .unwrap();
        let by_host_epoch: factory::DealResponse = app
            .wrap()
            .query_wasm_smart(
                factory_addr.clone(),
                &factory::QueryMsg::DealByHostEpoch {
                    host: host.to_string(),
                    epoch,
                },
            )
            .unwrap();
        assert_eq!(by_id.address, deal_addr.to_string());
        assert_eq!(by_host_epoch, by_id);
    }
    assert_eq!(cw20_balance!(app, other_token, deal_a1), Uint128::new(77));
}

#[test]
fn multiple_offers_get_sequential_ids_and_duplicate_host_epoch_is_permanent() {
    let mut app = AppBuilder::new()
        .with_stargate(EpochStargate)
        .build(|_, _, _| {});
    let owner = "owner".into_addr();
    let token_code_id = app.store_code(cw20_contract());
    let factory_code_id = app.store_code(factory_contract());
    let deal_code_id = app.store_code(deal_contract());
    let token = app
        .instantiate_contract(
            token_code_id,
            owner.clone(),
            &cw20_base::msg::InstantiateMsg {
                name: "Test USDT".to_string(),
                symbol: "USDT".to_string(),
                decimals: 6,
                initial_balances: vec![],
                mint: None,
                marketing: None,
            },
            &[],
            "test-usdt",
            None,
        )
        .unwrap();
    let factory_addr = app
        .instantiate_contract(
            factory_code_id,
            owner,
            &factory::InstantiateMsg {
                deal_code_id,
                settlement_cw20: token.to_string(),
                fee_recipient: "fees".into_addr().to_string(),
                fee_bps: 150,
            },
            &[],
            "marketplace-factory",
            None,
        )
        .unwrap();
    let host_a = "host-a".into_addr();
    let host_b = "host-b".into_addr();

    for (host, epoch) in [
        (host_a.clone(), 11),
        (host_a.clone(), 12),
        (host_b.clone(), 11),
    ] {
        app.execute_contract(
            host,
            factory_addr.clone(),
            &factory::ExecuteMsg::CreateOffer {
                target_epoch: epoch,
                price_micro_usdt_per_gnk: Uint128::new(1_000_000),
                buyer_budget_micro_usdt: Uint128::new(10_000_000),
            },
            &[],
        )
        .unwrap();
    }

    let listed: factory::ListDealsResponse = app
        .wrap()
        .query_wasm_smart(
            factory_addr.clone(),
            &factory::QueryMsg::ListDeals {
                start_after: None,
                limit: None,
            },
        )
        .unwrap();
    assert_eq!(
        listed.deals.iter().map(|deal| deal.id).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_ne!(listed.deals[0].address, listed.deals[1].address);
    assert_ne!(listed.deals[1].address, listed.deals[2].address);

    for (id, host, epoch) in [
        (1, host_a.clone(), 11),
        (2, host_a.clone(), 12),
        (3, host_b, 11),
    ] {
        let by_id: factory::DealResponse = app
            .wrap()
            .query_wasm_smart(factory_addr.clone(), &factory::QueryMsg::Deal { id })
            .unwrap();
        let by_host: factory::DealResponse = app
            .wrap()
            .query_wasm_smart(
                factory_addr.clone(),
                &factory::QueryMsg::DealByHostEpoch {
                    host: host.to_string(),
                    epoch,
                },
            )
            .unwrap();
        assert_eq!(by_id.address, by_host.address);
    }

    let duplicate = app
        .execute_contract(
            host_a,
            factory_addr.clone(),
            &factory::ExecuteMsg::CreateOffer {
                target_epoch: 11,
                price_micro_usdt_per_gnk: Uint128::new(2_000_000),
                buyer_budget_micro_usdt: Uint128::new(20_000_000),
            },
            &[],
        )
        .unwrap_err();
    assert!(duplicate
        .root_cause()
        .to_string()
        .contains("offer already exists"));
    let still_three: factory::ListDealsResponse = app
        .wrap()
        .query_wasm_smart(
            factory_addr,
            &factory::QueryMsg::ListDeals {
                start_after: None,
                limit: None,
            },
        )
        .unwrap();
    assert_eq!(still_three.deals.len(), 3);
}

#[test]
fn invalid_create_offer_and_native_funds_leave_factory_empty() {
    const DENOM: &str = "ngonka";
    let host = "host".into_addr();
    let funded_host = host.clone();
    let mut app =
        AppBuilder::new()
            .with_stargate(EpochStargate)
            .build(move |router, _, storage| {
                router
                    .bank
                    .init_balance(storage, &funded_host, coins(5, DENOM))
                    .unwrap();
            });
    let token_code_id = app.store_code(cw20_contract());
    let factory_code_id = app.store_code(factory_contract());
    let deal_code_id = app.store_code(deal_contract());
    let token = app
        .instantiate_contract(
            token_code_id,
            host.clone(),
            &cw20_base::msg::InstantiateMsg {
                name: "Test USDT".to_string(),
                symbol: "USDT".to_string(),
                decimals: 6,
                initial_balances: vec![],
                mint: None,
                marketing: None,
            },
            &[],
            "test-usdt",
            None,
        )
        .unwrap();
    let factory_addr = app
        .instantiate_contract(
            factory_code_id,
            host.clone(),
            &factory::InstantiateMsg {
                deal_code_id,
                settlement_cw20: token.to_string(),
                fee_recipient: "fees".into_addr().to_string(),
                fee_bps: 150,
            },
            &[],
            "marketplace-factory",
            None,
        )
        .unwrap();

    let funded_error = app
        .execute_contract(
            host.clone(),
            factory_addr.clone(),
            &factory::ExecuteMsg::CreateOffer {
                target_epoch: 11,
                price_micro_usdt_per_gnk: Uint128::one(),
                buyer_budget_micro_usdt: Uint128::one(),
            },
            &[coin(5, DENOM)],
        )
        .unwrap_err();
    assert!(funded_error.root_cause().to_string().contains("funds"));
    assert_eq!(
        app.wrap().query_balance(&host, DENOM).unwrap().amount,
        Uint128::new(5)
    );
    assert_eq!(
        app.wrap()
            .query_balance(&factory_addr, DENOM)
            .unwrap()
            .amount,
        Uint128::zero()
    );

    for msg in [
        factory::ExecuteMsg::CreateOffer {
            target_epoch: 11,
            price_micro_usdt_per_gnk: Uint128::zero(),
            buyer_budget_micro_usdt: Uint128::one(),
        },
        factory::ExecuteMsg::CreateOffer {
            target_epoch: 11,
            price_micro_usdt_per_gnk: Uint128::one(),
            buyer_budget_micro_usdt: Uint128::zero(),
        },
        factory::ExecuteMsg::CreateOffer {
            target_epoch: 10,
            price_micro_usdt_per_gnk: Uint128::one(),
            buyer_budget_micro_usdt: Uint128::one(),
        },
        factory::ExecuteMsg::CreateOffer {
            target_epoch: 51,
            price_micro_usdt_per_gnk: Uint128::one(),
            buyer_budget_micro_usdt: Uint128::one(),
        },
        factory::ExecuteMsg::CreateOffer {
            target_epoch: 11,
            price_micro_usdt_per_gnk: Uint128::new(1_000_000_001),
            buyer_budget_micro_usdt: Uint128::one(),
        },
    ] {
        assert!(app
            .execute_contract(host.clone(), factory_addr.clone(), &msg, &[])
            .is_err());
    }

    let listed: factory::ListDealsResponse = app
        .wrap()
        .query_wasm_smart(
            factory_addr.clone(),
            &factory::QueryMsg::ListDeals {
                start_after: None,
                limit: None,
            },
        )
        .unwrap();
    assert!(listed.deals.is_empty());
    assert!(app
        .wrap()
        .query_wasm_raw(factory_addr, b"pending_offer")
        .unwrap()
        .is_none());
}

#[test]
fn child_instantiate_failure_rolls_back_pending_registry_and_child() {
    let generated = Arc::new(Mutex::new(Vec::new()));
    let wasm = WasmKeeper::new().with_address_generator(RecordingAddressGenerator {
        generated: generated.clone(),
    });
    let mut app = AppBuilder::new()
        .with_wasm(wasm)
        .with_stargate(EpochStargate)
        .build(|_, _, _| {});
    let owner = "owner".into_addr();
    let token_code_id = app.store_code(cw20_contract());
    let factory_code_id = app.store_code(factory_contract());
    let failing_deal_code_id = app.store_code(failing_deal_contract());
    let token = app
        .instantiate_contract(
            token_code_id,
            owner.clone(),
            &cw20_base::msg::InstantiateMsg {
                name: "Test USDT".to_string(),
                symbol: "USDT".to_string(),
                decimals: 6,
                initial_balances: vec![],
                mint: None,
                marketing: None,
            },
            &[],
            "test-usdt",
            None,
        )
        .unwrap();
    let factory_addr = app
        .instantiate_contract(
            factory_code_id,
            owner,
            &factory::InstantiateMsg {
                deal_code_id: failing_deal_code_id,
                settlement_cw20: token.to_string(),
                fee_recipient: "fees".into_addr().to_string(),
                fee_bps: 150,
            },
            &[],
            "marketplace-factory",
            None,
        )
        .unwrap();
    generated.lock().unwrap().clear();

    let error = app
        .execute_contract(
            "host".into_addr(),
            factory_addr.clone(),
            &factory::ExecuteMsg::CreateOffer {
                target_epoch: 11,
                price_micro_usdt_per_gnk: Uint128::new(1_000_000),
                buyer_budget_micro_usdt: Uint128::new(10_000_000),
            },
            &[],
        )
        .unwrap_err();
    assert!(error
        .root_cause()
        .to_string()
        .contains("deliberate Deal instantiate failure"));
    let attempted_child = generated.lock().unwrap().last().unwrap().clone();
    assert!(app.contract_data(&attempted_child).is_err());
    assert!(app
        .wrap()
        .query_wasm_raw(factory_addr.clone(), b"pending_offer")
        .unwrap()
        .is_none());
    let storage = app.contract_storage(&factory_addr);
    assert_eq!(Item::<u64>::new("next_deal_id").load(&*storage).unwrap(), 1);
    drop(storage);
    let listed: factory::ListDealsResponse = app
        .wrap()
        .query_wasm_smart(
            factory_addr,
            &factory::QueryMsg::ListDeals {
                start_after: None,
                limit: None,
            },
        )
        .unwrap();
    assert!(listed.deals.is_empty());
}

#[test]
fn reply_id_overflow_rolls_back_child_pending_and_registry() {
    let generated = Arc::new(Mutex::new(Vec::new()));
    let wasm = WasmKeeper::new().with_address_generator(RecordingAddressGenerator {
        generated: generated.clone(),
    });
    let mut app = AppBuilder::new()
        .with_wasm(wasm)
        .with_stargate(EpochStargate)
        .build(|_, _, _| {});
    let owner = "owner".into_addr();
    let token_code_id = app.store_code(cw20_contract());
    let factory_code_id = app.store_code(factory_contract());
    let deal_code_id = app.store_code(deal_contract());
    let token = app
        .instantiate_contract(
            token_code_id,
            owner.clone(),
            &cw20_base::msg::InstantiateMsg {
                name: "Test USDT".to_string(),
                symbol: "USDT".to_string(),
                decimals: 6,
                initial_balances: vec![],
                mint: None,
                marketing: None,
            },
            &[],
            "test-usdt",
            None,
        )
        .unwrap();
    let factory_addr = app
        .instantiate_contract(
            factory_code_id,
            owner,
            &factory::InstantiateMsg {
                deal_code_id,
                settlement_cw20: token.to_string(),
                fee_recipient: "fees".into_addr().to_string(),
                fee_bps: 150,
            },
            &[],
            "marketplace-factory",
            None,
        )
        .unwrap();
    {
        let mut storage = app.contract_storage_mut(&factory_addr);
        Item::<u64>::new("next_deal_id")
            .save(&mut *storage, &u64::MAX)
            .unwrap();
    }
    generated.lock().unwrap().clear();

    let error = app
        .execute_contract(
            "host".into_addr(),
            factory_addr.clone(),
            &factory::ExecuteMsg::CreateOffer {
                target_epoch: 11,
                price_micro_usdt_per_gnk: Uint128::new(1_000_000),
                buyer_budget_micro_usdt: Uint128::new(10_000_000),
            },
            &[],
        )
        .unwrap_err();
    assert!(error.root_cause().to_string().contains("id overflow"));
    let attempted_child = generated.lock().unwrap().last().unwrap().clone();
    assert!(app.contract_data(&attempted_child).is_err());
    assert!(app
        .wrap()
        .query_wasm_raw(factory_addr.clone(), b"pending_offer")
        .unwrap()
        .is_none());
    let listed: factory::ListDealsResponse = app
        .wrap()
        .query_wasm_smart(
            factory_addr.clone(),
            &factory::QueryMsg::ListDeals {
                start_after: None,
                limit: None,
            },
        )
        .unwrap();
    assert!(listed.deals.is_empty());
    let storage = app.contract_storage(&factory_addr);
    assert_eq!(
        Item::<u64>::new("next_deal_id").load(&*storage).unwrap(),
        u64::MAX
    );
}

#[test]
fn rejected_native_and_cw20_funding_preserve_sender_balances() {
    const DENOM: &str = "ugonka";
    const STARTING_NATIVE: u128 = 200;
    const STARTING_CW20: u128 = 1_000;

    let owner = "owner".into_addr();
    let funded_owner = owner.clone();
    let mut app =
        AppBuilder::new()
            .with_stargate(EpochStargate)
            .build(move |router, _, storage| {
                router
                    .bank
                    .init_balance(storage, &funded_owner, coins(STARTING_NATIVE, DENOM))
                    .unwrap();
            });
    let cw20_code_id = app.store_code(cw20_contract());
    let factory_code_id = app.store_code(factory_contract());
    let deal_code_id = app.store_code(deal_contract());
    let token = app
        .instantiate_contract(
            cw20_code_id,
            owner.clone(),
            &cw20_base::msg::InstantiateMsg {
                name: "Test USDT".to_string(),
                symbol: "USDT".to_string(),
                decimals: 6,
                initial_balances: vec![Cw20Coin {
                    address: owner.to_string(),
                    amount: Uint128::new(STARTING_CW20),
                }],
                mint: None,
                marketing: None,
            },
            &[],
            "test-usdt",
            None,
        )
        .unwrap();

    let factory_error = app
        .instantiate_contract(
            factory_code_id,
            owner.clone(),
            &factory::InstantiateMsg {
                deal_code_id,
                settlement_cw20: token.to_string(),
                fee_recipient: "fees".into_addr().to_string(),
                fee_bps: 150,
            },
            &[coin(100, DENOM)],
            "marketplace-factory",
            None,
        )
        .unwrap_err();
    assert!(factory_error.root_cause().to_string().contains("funds"));
    assert_eq!(
        app.wrap().query_balance(&owner, DENOM).unwrap().amount,
        Uint128::new(STARTING_NATIVE)
    );

    let factory_addr = app
        .instantiate_contract(
            factory_code_id,
            owner.clone(),
            &factory::InstantiateMsg {
                deal_code_id,
                settlement_cw20: token.to_string(),
                fee_recipient: "fees".into_addr().to_string(),
                fee_bps: 150,
            },
            &[],
            "marketplace-factory",
            None,
        )
        .unwrap();
    let deal_msg = deal::InstantiateMsg {
        factory: factory_addr.to_string(),
        host: "host".into_addr().to_string(),
        target_epoch: 11,
        price_micro_usdt_per_gnk: Uint128::new(1_000_000),
        buyer_budget_micro_usdt: Uint128::new(STARTING_CW20),
        settlement_cw20: token.to_string(),
        fee_recipient: "fees".into_addr().to_string(),
        fee_bps: 150,
        pinned_gonka_sha: gonka_proto::SOURCE_COMMIT.to_string(),
    };
    app.send_tokens(owner.clone(), factory_addr.clone(), &[coin(100, DENOM)])
        .unwrap();
    let deal_error = app
        .instantiate_contract(
            deal_code_id,
            factory_addr.clone(),
            &deal_msg,
            &[coin(100, DENOM)],
            "marketplace-deal",
            None,
        )
        .unwrap_err();
    assert!(deal_error.root_cause().to_string().contains("funds"));
    assert_eq!(
        app.wrap()
            .query_balance(&factory_addr, DENOM)
            .unwrap()
            .amount,
        Uint128::new(100)
    );

    let deal_addr = app
        .instantiate_contract(
            deal_code_id,
            factory_addr,
            &deal_msg,
            &[],
            "marketplace-deal",
            None,
        )
        .unwrap();
    let funding_error = app
        .execute_contract(
            owner.clone(),
            token.clone(),
            &Cw20ExecuteMsg::Send {
                contract: deal_addr.to_string(),
                amount: Uint128::new(STARTING_CW20 - 1),
                msg: to_json_binary(&deal::Cw20HookMsg::Fund {}).unwrap(),
            },
            &[],
        )
        .unwrap_err();
    assert!(funding_error.root_cause().to_string().contains("exactly"));

    let owner_balance: BalanceResponse = app
        .wrap()
        .query_wasm_smart(
            token.clone(),
            &cw20::Cw20QueryMsg::Balance {
                address: owner.to_string(),
            },
        )
        .unwrap();
    let deal_balance: BalanceResponse = app
        .wrap()
        .query_wasm_smart(
            token,
            &cw20::Cw20QueryMsg::Balance {
                address: deal_addr.to_string(),
            },
        )
        .unwrap();
    assert_eq!(owner_balance.balance, Uint128::new(STARTING_CW20));
    assert_eq!(deal_balance.balance, Uint128::zero());
}

#[test]
fn c_recovery_before_deadline_preserves_ledgers_and_settles_once() {
    for settlement_epoch in [13, 14] {
        let (mut app, native, token, _, deal_addr, _, host, buyer, fees) = settlement_fixture!(
            false,
            true,
            FUNDING_BUDGET,
            1_000_000,
            "host",
            "buyer",
            "fees"
        );
        set_native_performance(&native, 11, host.to_string(), 60_000_000_000, 0, true);
        native.lock().unwrap().current_epoch = 13;
        native.lock().unwrap().performance_response = FundingQueryResponse::QueryFailure;
        assert!(
            native
                .lock()
                .unwrap()
                .performance_summary
                .as_ref()
                .unwrap()
                .claimed
        );
        let (before, balances) = assert_hidden_claim_preserves_state_and_balances!(
            app, token, deal_addr, buyer, host, fees
        );
        native.lock().unwrap().performance_response = FundingQueryResponse::Valid; // restored strictly before E+3
        assert!(
            native
                .lock()
                .unwrap()
                .performance_summary
                .as_ref()
                .unwrap()
                .claimed
        );
        // Exercise the recovered query before the deadline as well as at E+3.
        // Each iteration uses a fresh deal so settlement remains a one-time action.
        native.lock().unwrap().current_epoch = settlement_epoch;
        let error = app
            .execute_contract(
                "caller".into_addr(),
                deal_addr.clone(),
                &deal::ExecuteMsg::Refund {},
                &[],
            )
            .unwrap_err();
        assert_eq!(
            error.root_cause().to_string(),
            "native claim for target epoch 11 is already confirmed"
        );
        assert_eq!(
            app.wrap()
                .query_wasm_smart::<deal::StateResponse>(&deal_addr, &deal::QueryMsg::State {})
                .unwrap(),
            before
        );
        assert_eq!(
            [
                cw20_balance!(app, token, deal_addr),
                cw20_balance!(app, token, buyer),
                cw20_balance!(app, token, host),
                cw20_balance!(app, token, fees)
            ],
            balances
        );
        app.execute_contract(
            "caller".into_addr(),
            deal_addr.clone(),
            &deal::ExecuteMsg::SettleClaim {},
            &[],
        )
        .unwrap();
        let after: deal::StateResponse = app
            .wrap()
            .query_wasm_smart(&deal_addr, &deal::QueryMsg::State {})
            .unwrap();
        assert_eq!(after.status, deal::DealStatus::Releasing);
        // 60 GNK at 1 USDT/GNK, with a 1.5% fee, leaves 40 USDT unspent.
        assert_eq!(after.buyer_refund_usdt, Uint128::new(40_000_000));
        assert_eq!(after.host_net_usdt, Uint128::new(59_100_000));
        assert_eq!(after.fee_usdt, Uint128::new(900_000));
        assert_eq!(
            [
                cw20_balance!(app, token, deal_addr),
                cw20_balance!(app, token, buyer),
                cw20_balance!(app, token, host),
                cw20_balance!(app, token, fees)
            ],
            balances,
            "settlement records obligations without paying any role prematurely"
        );
        withdraw_all_usdt!(app, deal_addr);
        let paid = [
            cw20_balance!(app, token, deal_addr),
            cw20_balance!(app, token, buyer),
            cw20_balance!(app, token, host),
            cw20_balance!(app, token, fees),
        ];
        assert_eq!(paid[0], Uint128::zero());
        assert_eq!(paid[1] - balances[1], after.buyer_refund_usdt);
        assert_eq!(paid[2] - balances[2], after.host_net_usdt);
        assert_eq!(paid[3] - balances[3], after.fee_usdt);
        assert_eq!(
            after.buyer_refund_usdt + after.host_net_usdt + after.fee_usdt,
            Uint128::new(FUNDING_BUDGET)
        );
        let error = app
            .execute_contract(
                "repeat".into_addr(),
                deal_addr.clone(),
                &deal::ExecuteMsg::SettleClaim {},
                &[],
            )
            .unwrap_err();
        assert_eq!(
            error.root_cause().to_string(),
            "cannot settle claim in state Releasing; expected Locked"
        );
        assert_eq!(
            app.wrap()
                .query_wasm_smart::<deal::StateResponse>(&deal_addr, &deal::QueryMsg::State {})
                .unwrap(),
            after
        );
        assert_eq!(
            [
                cw20_balance!(app, token, deal_addr),
                cw20_balance!(app, token, buyer),
                cw20_balance!(app, token, host),
                cw20_balance!(app, token, fees)
            ],
            paid
        );
    }
    // Emit each coverage marker once, after both boundary cases pass. Native
    // and ledger markers cover only the modeled claimed summary and CW20
    // balances, not preservation of a native claim or vesting ledger.
    for suffix in [
        "native", "probe", "e2", "ledger", "refund", "settle", "repeat",
    ] {
        println!("C_CASE:r5-recover-{suffix}");
    }
}
