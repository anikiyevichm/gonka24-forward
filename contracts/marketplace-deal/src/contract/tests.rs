use std::marker::PhantomData;

use cosmwasm_std::{
    coin, from_json,
    testing::{message_info, mock_env, MockApi, MockQuerier, MockStorage},
    to_json_binary, Addr, Binary, ContractResult, Empty, GrpcQuery, Order, OwnedDeps, Querier,
    QuerierResult, QueryRequest, Record, Storage, SystemError, SystemResult, Uint128, Uint256,
    WasmQuery,
};
use cw2::get_contract_version;
use cw20::TokenInfoResponse;
use gonka_proto::{
    ClaimRecipientEntry, Coin, EpochPerformanceSummary,
    QueryEpochPerformanceSummaryByParticipantRequest,
    QueryEpochPerformanceSummaryByParticipantResponse, QueryGetCurrentEpochRequest,
    QueryGetCurrentEpochResponse, QueryListClaimRecipientsRequest,
    QueryListClaimRecipientsResponse, QueryTotalVestingAmountRequest,
    QueryTotalVestingAmountResponse,
};
use marketplace_api::deal::{DealStatus, GnkReleasePolicy, RefundReason};
use prost::Message;

use super::*;
use crate::error::GonkaQueryError;

const FACTORY: &str = "factory000";
const HOST: &str = "host000";
const TOKEN: &str = "token000";
const FEE_RECIPIENT: &str = "fees000";
const BUYER: &str = "buyer000";

fn test_addr(label: &str) -> Addr {
    MockApi::default().addr_make(label)
}

struct DealMockQuerier {
    base: MockQuerier<Empty>,
    current_epoch_response: Result<Binary, ()>,
    current_epoch_override: Option<QuerierResult>,
    routing_override: Option<QuerierResult>,
    claim_recipients_response: Result<Binary, ()>,
    performance_response: Result<Binary, ()>,
    performance_result_override: Option<QuerierResult>,
    remaining_vesting: Uint128,
    total_vesting_query_fails: bool,
}

impl Querier for DealMockQuerier {
    fn raw_query(&self, request: &[u8]) -> QuerierResult {
        let parsed: QueryRequest<Empty> = match from_json(request) {
            Ok(parsed) => parsed,
            Err(error) => {
                return SystemResult::Err(SystemError::InvalidRequest {
                    error: error.to_string(),
                    request: Binary::from(request),
                });
            }
        };

        match parsed {
            QueryRequest::Grpc(grpc) => self.handle_grpc(grpc),
            other => self.base.handle_query(&other),
        }
    }
}

impl DealMockQuerier {
    fn response(result: &Result<Binary, ()>) -> QuerierResult {
        match result {
            Ok(response) => SystemResult::Ok(ContractResult::Ok(response.clone())),
            Err(()) => SystemResult::Err(SystemError::UnsupportedRequest {
                kind: "mock grpc failure".to_string(),
            }),
        }
    }

    fn handle_grpc(&self, request: GrpcQuery) -> QuerierResult {
        match request.path.as_str() {
            marketplace_common::gonka::GET_CURRENT_EPOCH_PATH => {
                QueryGetCurrentEpochRequest::decode(request.data.as_slice()).unwrap();
                self.current_epoch_override
                    .clone()
                    .unwrap_or_else(|| Self::response(&self.current_epoch_response))
            }
            crate::gonka::LIST_CLAIM_RECIPIENTS_PATH => {
                let decoded =
                    QueryListClaimRecipientsRequest::decode(request.data.as_slice()).unwrap();
                assert_eq!(decoded.participant, test_addr(HOST).to_string());
                self.routing_override
                    .clone()
                    .unwrap_or_else(|| Self::response(&self.claim_recipients_response))
            }
            crate::gonka::EPOCH_PERFORMANCE_PATH => {
                let decoded = QueryEpochPerformanceSummaryByParticipantRequest::decode(
                    request.data.as_slice(),
                )
                .unwrap();
                assert_eq!(decoded.participant_id, test_addr(HOST).to_string());
                assert_eq!(decoded.epoch_index, 11);
                self.performance_result_override
                    .clone()
                    .unwrap_or_else(|| Self::response(&self.performance_response))
            }
            crate::gonka::TOTAL_VESTING_AMOUNT_PATH => {
                if self.total_vesting_query_fails {
                    return SystemResult::Err(SystemError::UnsupportedRequest {
                        kind: "deliberate total vesting failure".to_string(),
                    });
                }
                let decoded =
                    QueryTotalVestingAmountRequest::decode(request.data.as_slice()).unwrap();
                assert_eq!(
                    decoded.participant_address,
                    mock_env().contract.address.to_string()
                );
                let response = QueryTotalVestingAmountResponse {
                    total_amount: if self.remaining_vesting.is_zero() {
                        vec![]
                    } else {
                        vec![Coin {
                            denom: crate::gonka::NGONKA_DENOM.to_string(),
                            amount: self.remaining_vesting.to_string(),
                        }]
                    },
                }
                .encode_to_vec();
                SystemResult::Ok(ContractResult::Ok(Binary::from(response)))
            }
            _ => SystemResult::Err(SystemError::UnsupportedRequest {
                kind: "unexpected grpc query".to_string(),
            }),
        }
    }
}

fn mock_deps(
    current_epoch: u64,
    token_decimals: u8,
) -> OwnedDeps<MockStorage, MockApi, DealMockQuerier, Empty> {
    let contract = mock_env().contract.address;
    let balances = [(contract.as_str(), &[coin(50, "ngonka")][..])];
    let mut base = MockQuerier::new(&balances);
    let token = test_addr(TOKEN).to_string();
    base.update_wasm(move |query| match query {
        WasmQuery::Smart { contract_addr, .. } if contract_addr == &token => {
            SystemResult::Ok(ContractResult::Ok(
                to_json_binary(&TokenInfoResponse {
                    name: "Test USDT".to_string(),
                    symbol: "USDT".to_string(),
                    decimals: token_decimals,
                    total_supply: Uint128::new(1_000_000),
                })
                .unwrap(),
            ))
        }
        _ => SystemResult::Err(SystemError::UnsupportedRequest {
            kind: "unexpected wasm query".to_string(),
        }),
    });
    OwnedDeps {
        storage: MockStorage::default(),
        api: MockApi::default(),
        querier: DealMockQuerier {
            base,
            current_epoch_response: Ok(Binary::from(
                QueryGetCurrentEpochResponse {
                    epoch: current_epoch,
                }
                .encode_to_vec(),
            )),
            claim_recipients_response: Ok(Binary::from(
                QueryListClaimRecipientsResponse {
                    entries: vec![ClaimRecipientEntry {
                        epoch: 11,
                        recipient: mock_env().contract.address.to_string(),
                    }],
                }
                .encode_to_vec(),
            )),
            performance_response: Ok(Binary::from(
                QueryEpochPerformanceSummaryByParticipantResponse {
                    epoch_performance_summary: Some(EpochPerformanceSummary {
                        epoch_index: 11,
                        participant_id: test_addr(HOST).to_string(),
                        claimed: true,
                        ..EpochPerformanceSummary::default()
                    }),
                }
                .encode_to_vec(),
            )),
            performance_result_override: None,
            current_epoch_override: None,
            routing_override: None,
            remaining_vesting: Uint128::new(70),
            total_vesting_query_fails: false,
        },
        custom_query_type: PhantomData,
    }
}

fn valid_instantiate_msg() -> InstantiateMsg {
    InstantiateMsg {
        factory: test_addr(FACTORY).to_string(),
        host: test_addr(HOST).to_string(),
        target_epoch: 11,
        price_micro_usdt_per_gnk: Uint128::new(1_000_000),
        buyer_budget_micro_usdt: Uint128::new(100_000_000),
        settlement_cw20: test_addr(TOKEN).to_string(),
        fee_recipient: test_addr(FEE_RECIPIENT).to_string(),
        fee_bps: PROTOCOL_FEE_BPS,
        pinned_gonka_sha: gonka_proto::SOURCE_COMMIT.to_string(),
    }
}

fn instantiate_valid(deps: DepsMut) {
    instantiate(
        deps,
        mock_env(),
        message_info(&test_addr(FACTORY), &[]),
        valid_instantiate_msg(),
    )
    .unwrap();
}

#[test]
fn instantiate_stores_validated_config_and_zero_open_state() {
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    let response = instantiate(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr(FACTORY), &[]),
        valid_instantiate_msg(),
    )
    .unwrap();

    assert!(response.messages.is_empty());
    let config = CONFIG.load(&deps.storage).unwrap();
    assert_eq!(config.factory, test_addr(FACTORY));
    assert_eq!(config.host, test_addr(HOST));
    assert_eq!(config.funded_capacity_ngonka, Uint128::new(100_000_000_000));
    let state = STATE.load(&deps.storage).unwrap();
    assert_eq!(state, DealState::open());
    let version = get_contract_version(&deps.storage).unwrap();
    assert_eq!(version.contract, CONTRACT_NAME);
    assert_eq!(version.version, CONTRACT_VERSION);

    let config_response: ConfigResponse =
        from_json(query(deps.as_ref(), mock_env(), QueryMsg::Config {}).unwrap()).unwrap();
    assert_eq!(
        config_response.deal_address,
        mock_env().contract.address.to_string()
    );
    assert_eq!(
        config_response.funded_capacity_ngonka,
        config.funded_capacity_ngonka
    );

    let state_response: StateResponse =
        from_json(query(deps.as_ref(), mock_env(), QueryMsg::State {}).unwrap()).unwrap();
    assert_eq!(state_response.status, DealStatus::Open);
    assert_eq!(state_response.buyer, None);
    assert_eq!(state_response.total_claim_ngonka, Uint128::zero());

    let native: NativeStatusResponse =
        from_json(query(deps.as_ref(), mock_env(), QueryMsg::NativeStatus {}).unwrap()).unwrap();
    assert_eq!(native.remaining_vesting_ngonka, Uint128::new(70));
    assert_eq!(native.liquid_balance_ngonka, Uint128::new(50));
}

#[test]
fn instantiate_rejects_funds_and_untrusted_factory() {
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    assert!(matches!(
        instantiate(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr(FACTORY), &[coin(1, "ngonka")]),
            valid_instantiate_msg(),
        ),
        Err(ContractError::Payment(_))
    ));

    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    assert_eq!(
        instantiate(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("attacker000"), &[]),
            valid_instantiate_msg(),
        )
        .unwrap_err(),
        ContractError::UnauthorizedFactory
    );
    assert!(CONFIG.may_load(&deps.storage).unwrap().is_none());
}

#[test]
fn instantiate_rejects_invalid_financial_and_source_configuration() {
    let cases = [
        (
            {
                let mut msg = valid_instantiate_msg();
                msg.fee_bps = 149;
                msg
            },
            ContractError::InvalidFeeBps {
                expected: PROTOCOL_FEE_BPS,
                actual: 149,
            },
        ),
        (
            {
                let mut msg = valid_instantiate_msg();
                msg.price_micro_usdt_per_gnk = Uint128::zero();
                msg
            },
            ContractError::ZeroPrice,
        ),
        (
            {
                let mut msg = valid_instantiate_msg();
                msg.buyer_budget_micro_usdt = Uint128::zero();
                msg
            },
            ContractError::ZeroBudget,
        ),
        (
            {
                let mut msg = valid_instantiate_msg();
                msg.pinned_gonka_sha = "wrong".to_string();
                msg
            },
            ContractError::PinnedGonkaShaMismatch,
        ),
        (
            {
                let mut msg = valid_instantiate_msg();
                msg.buyer_budget_micro_usdt = Uint128::one();
                msg.price_micro_usdt_per_gnk = Uint128::new(1_000_000_001);
                msg
            },
            ContractError::ZeroCapacity,
        ),
        (
            {
                let mut msg = valid_instantiate_msg();
                msg.buyer_budget_micro_usdt = Uint128::MAX;
                msg.price_micro_usdt_per_gnk = Uint128::one();
                msg
            },
            ContractError::Math(marketplace_common::error::MathError::ArithmeticOverflow {
                operation: "funded capacity conversion to Uint128",
            }),
        ),
    ];

    for (msg, expected) in cases {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        assert_eq!(
            instantiate(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr(FACTORY), &[]),
                msg,
            )
            .unwrap_err(),
            expected
        );
    }

    let mut deps = mock_deps(10, 18);
    assert_eq!(
        instantiate(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr(FACTORY), &[]),
            valid_instantiate_msg(),
        )
        .unwrap_err(),
        ContractError::InvalidSettlementDecimals {
            expected: SETTLEMENT_TOKEN_DECIMALS,
            actual: 18,
        }
    );

    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    deps.querier.base.update_wasm(|_| {
        SystemResult::Err(SystemError::UnsupportedRequest {
            kind: "missing token".to_string(),
        })
    });
    assert_eq!(
        instantiate(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr(FACTORY), &[]),
            valid_instantiate_msg(),
        )
        .unwrap_err(),
        ContractError::InvalidSettlementToken
    );

    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    let mut msg = valid_instantiate_msg();
    msg.host = "x".to_string();
    assert!(matches!(
        instantiate(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr(FACTORY), &[]),
            msg,
        ),
        Err(ContractError::Std(_))
    ));
}

#[test]
fn instantiate_enforces_epoch_boundaries_with_checked_lookahead() {
    let mut msg = valid_instantiate_msg();
    msg.target_epoch = 10;
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    assert_eq!(
        instantiate(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr(FACTORY), &[]),
            msg,
        )
        .unwrap_err(),
        ContractError::InvalidTargetEpoch {
            current: 10,
            target: 10,
        }
    );

    let mut msg = valid_instantiate_msg();
    msg.target_epoch = 51;
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    assert_eq!(
        instantiate(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr(FACTORY), &[]),
            msg,
        )
        .unwrap_err(),
        ContractError::EpochLookaheadExceeded {
            target: 51,
            maximum: 50,
        }
    );

    let mut msg = valid_instantiate_msg();
    msg.target_epoch = u64::MAX - 9;
    let mut deps = mock_deps(u64::MAX - 10, SETTLEMENT_TOKEN_DECIMALS);
    assert_eq!(
        instantiate(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr(FACTORY), &[]),
            msg,
        )
        .unwrap_err(),
        ContractError::EpochLookaheadOverflow
    );

    for target in [11, 50] {
        let mut msg = valid_instantiate_msg();
        msg.target_epoch = target;
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr(FACTORY), &[]),
            msg,
        )
        .unwrap();
    }
}

fn fund_receive(sender: &str, amount: u128, msg: Binary) -> ExecuteMsg {
    ExecuteMsg::Receive(Cw20ReceiveMsg {
        sender: sender.to_string(),
        amount: Uint128::new(amount),
        msg,
    })
}

fn exact_fund_receive(sender: &Addr) -> ExecuteMsg {
    fund_receive(
        sender.as_str(),
        valid_instantiate_msg().buyer_budget_micro_usdt.u128(),
        to_json_binary(&Cw20HookMsg::Fund {}).unwrap(),
    )
}

#[test]
fn refund_rejects_native_funds_before_reading_or_changing_state() {
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    instantiate_valid(deps.as_mut());
    let original = STATE.load(&deps.storage).unwrap();

    assert!(matches!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr(BUYER), &[coin(1, "ngonka")]),
            ExecuteMsg::Refund {},
        ),
        Err(ContractError::Payment(_))
    ));
    assert_eq!(STATE.load(&deps.storage).unwrap(), original);
}

fn set_performance(
    deps: &mut OwnedDeps<MockStorage, MockApi, DealMockQuerier, Empty>,
    epoch: u64,
    participant: String,
    earned_coins: u64,
    rewarded_coins: u64,
    claimed: bool,
) {
    deps.querier.performance_response = Ok(Binary::from(
        QueryEpochPerformanceSummaryByParticipantResponse {
            epoch_performance_summary: Some(EpochPerformanceSummary {
                epoch_index: epoch,
                participant_id: participant,
                earned_coins,
                rewarded_coins,
                claimed,
                ..EpochPerformanceSummary::default()
            }),
        }
        .encode_to_vec(),
    ));
}

fn prepare_locked(
    deps: &mut OwnedDeps<MockStorage, MockApi, DealMockQuerier, Empty>,
    funded: bool,
) {
    instantiate_valid(deps.as_mut());
    if funded {
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr(TOKEN), &[]),
            exact_fund_receive(&test_addr(BUYER)),
        )
        .unwrap();
    }
    set_current_epoch(deps, 11);
    execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr("keeper"), &[]),
        ExecuteMsg::Lock {},
    )
    .unwrap();
}

fn assert_cw20_transfer(
    message: &cosmwasm_std::SubMsg,
    token: &Addr,
    recipient: &Addr,
    amount: Uint128,
) {
    let cosmwasm_std::CosmosMsg::Wasm(WasmMsg::Execute {
        contract_addr,
        msg,
        funds,
    }) = &message.msg
    else {
        panic!("expected CW20 Wasm execute message");
    };
    assert_eq!(contract_addr, token.as_str());
    assert!(funds.is_empty());
    assert_eq!(
        from_json::<Cw20ExecuteMsg>(msg).unwrap(),
        Cw20ExecuteMsg::Transfer {
            recipient: recipient.to_string(),
            amount,
        }
    );
}

fn assert_bank_send(message: &cosmwasm_std::SubMsg, recipient: &Addr, amount: Uint128) {
    let cosmwasm_std::CosmosMsg::Bank(BankMsg::Send {
        to_address,
        amount: coins,
    }) = &message.msg
    else {
        panic!("expected native bank send message");
    };
    assert_eq!(to_address, recipient.as_str());
    assert_eq!(coins, &vec![coin(amount.u128(), NGONKA_DENOM)]);
}

fn save_releasing_state(
    deps: &mut OwnedDeps<MockStorage, MockApi, DealMockQuerier, Empty>,
    buyer: Option<Addr>,
    buyer_entitlement_ngonka: u128,
) {
    let total_claim_ngonka = Uint128::new(100);
    STATE
        .save(
            deps.as_mut().storage,
            &DealState {
                status: DealStatus::Releasing,
                refund_reason: None,
                buyer,
                recipient_locked: true,
                work_ngonka: Uint128::new(40),
                reward_ngonka: Uint128::new(60),
                total_claim_ngonka,
                buyer_entitlement_ngonka: Uint128::new(buyer_entitlement_ngonka),
                host_entitlement_ngonka: total_claim_ngonka
                    .checked_sub(Uint128::new(buyer_entitlement_ngonka))
                    .unwrap(),
                gnk_release_policy: GnkReleasePolicy::Proportional {
                    buyer_share_numerator: Uint128::new(buyer_entitlement_ngonka),
                    share_denominator: total_claim_ngonka,
                },
                released_total_ngonka: Uint256::zero(),
                buyer_released_ngonka: Uint256::zero(),
                host_released_ngonka: Uint256::zero(),
                gross_usdt: Uint128::zero(),
                fee_usdt: Uint128::zero(),
                host_net_usdt: Uint128::zero(),
                buyer_refund_usdt: Uint128::zero(),
            },
        )
        .unwrap();
}

fn save_host_only_state(
    deps: &mut OwnedDeps<MockStorage, MockApi, DealMockQuerier, Empty>,
    status: DealStatus,
    buyer: Option<Addr>,
) {
    let mut state = DealState::open();
    state.status = status;
    state.buyer = buyer;
    state.recipient_locked = true;
    state.gnk_release_policy = GnkReleasePolicy::HostOnly;
    STATE.save(deps.as_mut().storage, &state).unwrap();
}

#[test]
fn settle_funded_positive_claim_freezes_accounting_without_transfers() {
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    prepare_locked(&mut deps, true);
    let config_before = CONFIG.load(&deps.storage).unwrap();
    set_performance(
        &mut deps,
        11,
        test_addr(HOST).to_string(),
        50_000_000_000,
        2_000_000_000,
        true,
    );

    let response = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr("permissionless-caller"), &[]),
        ExecuteMsg::SettleClaim {},
    )
    .unwrap();

    assert_eq!(CONFIG.load(&deps.storage).unwrap(), config_before);
    let state = STATE.load(&deps.storage).unwrap();
    assert_eq!(state.status, DealStatus::Releasing);
    assert!(state.recipient_locked);
    assert_eq!(state.buyer, Some(test_addr(BUYER)));
    assert_eq!(state.work_ngonka, Uint128::new(50_000_000_000));
    assert_eq!(state.reward_ngonka, Uint128::new(2_000_000_000));
    assert_eq!(state.total_claim_ngonka, Uint128::new(52_000_000_000));
    assert_eq!(state.buyer_entitlement_ngonka, Uint128::new(52_000_000_000));
    assert_eq!(state.host_entitlement_ngonka, Uint128::zero());
    assert_eq!(
        state.gnk_release_policy,
        GnkReleasePolicy::Proportional {
            buyer_share_numerator: Uint128::new(52_000_000_000),
            share_denominator: Uint128::new(52_000_000_000),
        }
    );
    assert_eq!(state.released_total_ngonka, Uint256::zero());
    assert_eq!(state.buyer_released_ngonka, Uint256::zero());
    assert_eq!(state.host_released_ngonka, Uint256::zero());
    assert_eq!(state.gross_usdt, Uint128::new(52_000_000));
    assert_eq!(state.fee_usdt, Uint128::new(780_000));
    assert_eq!(state.host_net_usdt, Uint128::new(51_220_000));
    assert_eq!(state.buyer_refund_usdt, Uint128::new(48_000_000));

    assert!(response.messages.is_empty());
    let payments = query_usdt_payments(deps.as_ref()).unwrap();
    assert_eq!(payments.host.pending_micro_usdt, state.host_net_usdt);
    assert_eq!(payments.fee.pending_micro_usdt, state.fee_usdt);
    assert_eq!(payments.buyer.pending_micro_usdt, state.buyer_refund_usdt);
    assert_eq!(payments.host.paid_micro_usdt, Uint128::zero());
    assert_event_attributes(
        &response,
        "claim_settled",
        &[
            ("deal", mock_env().contract.address.to_string()),
            ("host", test_addr(HOST).to_string()),
            ("target_epoch", "11".to_string()),
            ("work_ngonka", "50000000000".to_string()),
            ("reward_ngonka", "2000000000".to_string()),
            ("total_claim_ngonka", "52000000000".to_string()),
            ("gross_usdt", "52000000".to_string()),
            ("fee_usdt", "780000".to_string()),
            ("host_net_usdt", "51220000".to_string()),
            ("buyer_refund_usdt", "48000000".to_string()),
        ],
    );
    assert!(!response
        .events
        .iter()
        .any(|event| matches!(event.ty.as_str(), "usdt_paid" | "usdt_refunded")));
    assert!(!response
        .events
        .iter()
        .any(|event| event.ty == "deal_completed"));
}

#[test]
fn settle_no_sale_positive_claim_assigns_all_gnk_to_host_without_usdt_messages() {
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    prepare_locked(&mut deps, false);
    let config_before = CONFIG.load(&deps.storage).unwrap();
    set_performance(&mut deps, 11, test_addr(HOST).to_string(), 7, 5, true);

    let response = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr("caller"), &[]),
        ExecuteMsg::SettleClaim {},
    )
    .unwrap();

    assert!(response.messages.is_empty());
    assert_eq!(CONFIG.load(&deps.storage).unwrap(), config_before);
    let state = STATE.load(&deps.storage).unwrap();
    assert_eq!(state.status, DealStatus::Releasing);
    assert_eq!(state.buyer, None);
    assert_eq!(state.total_claim_ngonka, Uint128::new(12));
    assert_eq!(state.buyer_entitlement_ngonka, Uint128::zero());
    assert_eq!(state.host_entitlement_ngonka, Uint128::new(12));
    assert_eq!(
        state.gnk_release_policy,
        GnkReleasePolicy::Proportional {
            buyer_share_numerator: Uint128::zero(),
            share_denominator: Uint128::new(12),
        }
    );
    assert_eq!(state.gross_usdt, Uint128::zero());
    assert_eq!(state.fee_usdt, Uint128::zero());
    assert_eq!(state.host_net_usdt, Uint128::zero());
    assert_eq!(state.buyer_refund_usdt, Uint128::zero());
    assert!(!response
        .events
        .iter()
        .any(|event| matches!(event.ty.as_str(), "usdt_paid" | "usdt_refunded")));
}

#[test]
fn settle_zero_claim_completes_directly_and_refunds_only_a_funded_buyer() {
    for funded in [true, false] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        prepare_locked(&mut deps, funded);
        set_performance(&mut deps, 11, test_addr(HOST).to_string(), 0, 0, true);

        let response = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("caller"), &[]),
            ExecuteMsg::SettleClaim {},
        )
        .unwrap();
        let state = STATE.load(&deps.storage).unwrap();
        assert_eq!(state.status, DealStatus::Completed);
        assert_eq!(state.work_ngonka, Uint128::zero());
        assert_eq!(state.reward_ngonka, Uint128::zero());
        assert_eq!(state.total_claim_ngonka, Uint128::zero());
        assert_eq!(state.buyer_entitlement_ngonka, Uint128::zero());
        assert_eq!(state.host_entitlement_ngonka, Uint128::zero());
        assert_eq!(state.gnk_release_policy, GnkReleasePolicy::HostOnly);
        assert_eq!(state.released_total_ngonka, Uint256::zero());
        assert_eq!(state.buyer_released_ngonka, Uint256::zero());
        assert_eq!(state.host_released_ngonka, Uint256::zero());
        assert_eq!(state.gross_usdt, Uint128::zero());
        assert_eq!(state.fee_usdt, Uint128::zero());
        assert_eq!(state.host_net_usdt, Uint128::zero());
        assert_eq!(
            state.buyer_refund_usdt,
            if funded {
                Uint128::new(100_000_000)
            } else {
                Uint128::zero()
            }
        );
        assert!(response.messages.is_empty());
        assert!(!response
            .events
            .iter()
            .any(|event| matches!(event.ty.as_str(), "usdt_paid" | "usdt_refunded")));
        assert_eq!(
            query_usdt_payments(deps.as_ref())
                .unwrap()
                .buyer
                .pending_micro_usdt,
            state.buyer_refund_usdt
        );
        assert!(response
            .events
            .iter()
            .any(|event| event.ty == "claim_settled"));
        assert!(response
            .events
            .iter()
            .any(|event| event.ty == "deal_completed"));

        let completed = state.clone();
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("another-caller"), &[]),
                ExecuteMsg::SettleClaim {},
            )
            .unwrap_err(),
            ContractError::InvalidSettlementState {
                actual: DealStatus::Completed,
            }
        );
        assert_eq!(STATE.load(&deps.storage).unwrap(), completed);
    }
}

#[test]
fn settle_requires_locked_proof_confirmed_exact_summary_and_no_native_funds() {
    let mut open_deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    instantiate_valid(open_deps.as_mut());
    assert_eq!(
        execute(
            open_deps.as_mut(),
            mock_env(),
            message_info(&test_addr("caller"), &[]),
            ExecuteMsg::SettleClaim {},
        )
        .unwrap_err(),
        ContractError::InvalidSettlementState {
            actual: DealStatus::Open,
        }
    );

    let mut no_proof = DealState::open();
    no_proof.status = DealStatus::Locked;
    STATE.save(&mut open_deps.storage, &no_proof).unwrap();
    assert_eq!(
        execute(
            open_deps.as_mut(),
            mock_env(),
            message_info(&test_addr("caller"), &[]),
            ExecuteMsg::SettleClaim {},
        )
        .unwrap_err(),
        ContractError::MissingRecipientLockProof
    );

    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    prepare_locked(&mut deps, true);
    let locked = STATE.load(&deps.storage).unwrap();
    set_performance(&mut deps, 11, test_addr(HOST).to_string(), 1, 2, false);
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("caller"), &[]),
            ExecuteMsg::SettleClaim {},
        )
        .unwrap_err(),
        ContractError::ClaimNotConfirmed { epoch: 11 }
    );
    assert_eq!(STATE.load(&deps.storage).unwrap(), locked);

    deps.querier.performance_response = Err(());
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("caller"), &[]),
            ExecuteMsg::SettleClaim {},
        )
        .unwrap_err(),
        ContractError::Gonka(GonkaQueryError::UnsupportedRequest {
            route: crate::gonka::EPOCH_PERFORMANCE_PATH,
        })
    );
    assert_eq!(STATE.load(&deps.storage).unwrap(), locked);

    assert!(matches!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("caller"), &[coin(1, "ngonka")]),
            ExecuteMsg::SettleClaim {},
        ),
        Err(ContractError::Payment(_))
    ));
    assert_eq!(STATE.load(&deps.storage).unwrap(), locked);
}

fn set_current_epoch(
    deps: &mut OwnedDeps<MockStorage, MockApi, DealMockQuerier, Empty>,
    epoch: u64,
) {
    deps.querier.current_epoch_response = Ok(Binary::from(
        QueryGetCurrentEpochResponse { epoch }.encode_to_vec(),
    ));
}

fn set_claim_recipients(
    deps: &mut OwnedDeps<MockStorage, MockApi, DealMockQuerier, Empty>,
    entries: Vec<ClaimRecipientEntry>,
) {
    deps.querier.claim_recipients_response = Ok(Binary::from(
        QueryListClaimRecipientsResponse { entries }.encode_to_vec(),
    ));
}

fn assert_event_attributes(response: &Response, event_type: &str, expected: &[(&str, String)]) {
    let event = response
        .events
        .iter()
        .find(|event| event.ty == event_type)
        .unwrap();
    for (key, value) in expected {
        assert!(event
            .attributes
            .iter()
            .any(|attribute| attribute.key == *key && attribute.value == *value));
    }
}

// Direct execute calls have no transaction rollback: include every key, even
// keys introduced later, when proving that a rejected call made no writes.
fn storage_snapshot(storage: &dyn Storage) -> Vec<Record> {
    storage.range(None, None, Order::Ascending).collect()
}

fn assert_locked(storage: &dyn Storage, response: &Response, mut before: DealState) {
    before.status = DealStatus::Locked;
    before.recipient_locked = true;
    assert_eq!(STATE.load(storage).unwrap(), before);
    assert!(response.messages.is_empty());
    assert_event_attributes(
        response,
        "deal_locked",
        &[
            ("deal", mock_env().contract.address.to_string()),
            ("host", test_addr(HOST).to_string()),
            ("target_epoch", "11".to_string()),
        ],
    );
    let buyer_attribute = response.events[0]
        .attributes
        .iter()
        .find(|attribute| attribute.key == "buyer");
    assert_eq!(
        buyer_attribute.map(|attribute| attribute.value.clone()),
        before.buyer.map(|buyer| buyer.to_string())
    );
}

fn assert_network_unconfirmed_refund(
    storage: &dyn Storage,
    response: &Response,
    mut before: DealState,
) {
    let config = CONFIG.load(storage).unwrap();
    let amount = config.buyer_budget_micro_usdt;
    before.status = DealStatus::Refunded;
    before.refund_reason = Some(RefundReason::NetworkUnconfirmed);
    before.gnk_release_policy = GnkReleasePolicy::HostOnly;
    before.buyer_refund_usdt = amount;
    assert_eq!(STATE.load(storage).unwrap(), before);
    assert_eq!(response.messages.len(), 1);
    assert_cw20_transfer(
        &response.messages[0],
        &test_addr(TOKEN),
        &test_addr(BUYER),
        amount,
    );
    for (event, recipient_key) in [("deal_refunded", "buyer"), ("usdt_refunded", "recipient")] {
        assert_event_attributes(
            response,
            event,
            &[
                ("deal", mock_env().contract.address.to_string()),
                ("host", test_addr(HOST).to_string()),
                ("target_epoch", config.target_epoch.to_string()),
                (recipient_key, test_addr(BUYER).to_string()),
                ("reason", "network_unconfirmed".to_string()),
                ("amount_micro_usdt", amount.to_string()),
            ],
        );
    }
    assert_event_attributes(
        response,
        "deal_refunded",
        &[("status", "refunded".to_string())],
    );
}

#[test]
fn lock_open_no_sale_and_funded_preserve_config_accounting_and_emit_event() {
    for funded in [false, true] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate_valid(deps.as_mut());
        if funded {
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr(TOKEN), &[]),
                exact_fund_receive(&test_addr(BUYER)),
            )
            .unwrap();
        }
        set_current_epoch(&mut deps, 11);
        let config_before = CONFIG.load(&deps.storage).unwrap();
        let state_before = STATE.load(&deps.storage).unwrap();

        let response = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("keeper"), &[]),
            ExecuteMsg::Lock {},
        )
        .unwrap();

        assert_eq!(CONFIG.load(&deps.storage).unwrap(), config_before);
        assert_locked(&deps.storage, &response, state_before);
    }
}

#[test]
fn lock_enforces_exact_epoch_window_without_mutation() {
    for (current, expected) in [
        (
            10,
            Some(ContractError::LockWindowNotOpen {
                current: 10,
                target: 11,
            }),
        ),
        (11, None),
        (15, None),
        (
            16,
            Some(ContractError::LockWindowClosed {
                current: 16,
                target: 11,
                end_exclusive: 16,
            }),
        ),
    ] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate_valid(deps.as_mut());
        set_current_epoch(&mut deps, current);
        let before = STATE.load(&deps.storage).unwrap();
        let result = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("caller"), &[]),
            ExecuteMsg::Lock {},
        );
        match expected {
            Some(error) => {
                assert_eq!(result.unwrap_err(), error);
                assert_eq!(STATE.load(&deps.storage).unwrap(), before);
            }
            None => assert_eq!(
                STATE.load(&deps.storage).unwrap().status,
                DealStatus::Locked
            ),
        }
    }
}

#[test]
fn lock_requires_valid_unique_exact_routing_and_rejects_query_failures() {
    let deal = mock_env().contract.address.to_string();
    let cases = [
        (
            Ok(Binary::from(
                QueryListClaimRecipientsResponse { entries: vec![] }.encode_to_vec(),
            )),
            GonkaQueryError::ClaimRecipientNotFound { epoch: 11 },
        ),
        (
            Ok(Binary::from(
                QueryListClaimRecipientsResponse {
                    entries: vec![ClaimRecipientEntry {
                        epoch: 11,
                        recipient: test_addr("other").to_string(),
                    }],
                }
                .encode_to_vec(),
            )),
            GonkaQueryError::IdentityMismatch {
                route: crate::gonka::LIST_CLAIM_RECIPIENTS_PATH,
                field: "recipient",
            },
        ),
        (
            Ok(Binary::from(
                QueryListClaimRecipientsResponse {
                    entries: vec![ClaimRecipientEntry {
                        epoch: 11,
                        recipient: "x".to_string(),
                    }],
                }
                .encode_to_vec(),
            )),
            GonkaQueryError::InvalidAddress {
                route: crate::gonka::LIST_CLAIM_RECIPIENTS_PATH,
                field: "recipient",
            },
        ),
        (
            Ok(Binary::from(
                QueryListClaimRecipientsResponse {
                    entries: vec![
                        ClaimRecipientEntry {
                            epoch: 11,
                            recipient: deal.clone(),
                        },
                        ClaimRecipientEntry {
                            epoch: 11,
                            recipient: deal.clone(),
                        },
                    ],
                }
                .encode_to_vec(),
            )),
            GonkaQueryError::DuplicateClaimRecipient { epoch: 11 },
        ),
        (
            Ok(Binary::from(
                QueryListClaimRecipientsResponse {
                    entries: (0..=crate::gonka::MAX_CLAIM_RECIPIENT_ENTRIES)
                        .map(|epoch| ClaimRecipientEntry {
                            epoch: epoch as u64,
                            recipient: deal.clone(),
                        })
                        .collect(),
                }
                .encode_to_vec(),
            )),
            GonkaQueryError::TooManyClaimRecipients {
                actual: crate::gonka::MAX_CLAIM_RECIPIENT_ENTRIES + 1,
                maximum: crate::gonka::MAX_CLAIM_RECIPIENT_ENTRIES,
            },
        ),
    ];

    for (response, expected) in cases {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate_valid(deps.as_mut());
        set_current_epoch(&mut deps, 11);
        deps.querier.claim_recipients_response = response;
        let before = STATE.load(&deps.storage).unwrap();
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("caller"), &[]),
                ExecuteMsg::Lock {},
            )
            .unwrap_err(),
            ContractError::Gonka(expected)
        );
        assert_eq!(STATE.load(&deps.storage).unwrap(), before);
    }

    for response in [
        Err(()),
        Ok(Binary::from(vec![0xff])),
        Ok(Binary::from(vec![
            0;
            crate::gonka::MAX_GRPC_RESPONSE_BYTES + 1
        ])),
    ] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate_valid(deps.as_mut());
        set_current_epoch(&mut deps, 11);
        deps.querier.claim_recipients_response = response;
        let before = STATE.load(&deps.storage).unwrap();
        assert!(matches!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("caller"), &[]),
                ExecuteMsg::Lock {},
            ),
            Err(ContractError::Gonka(
                GonkaQueryError::QueryFailed { .. }
                    | GonkaQueryError::DecodeFailed { .. }
                    | GonkaQueryError::ResponseTooLarge { .. }
            ))
        ));
        assert_eq!(STATE.load(&deps.storage).unwrap(), before);
    }
}

#[test]
fn cancel_authorization_window_and_routing_proof_are_fail_closed() {
    for (current, caller, recipient, expected) in [
        (10, HOST, None, None),
        (
            10,
            "other",
            None,
            Some(ContractError::CancelBeforeTargetRequiresHost { target: 11 }),
        ),
        (11, "other", None, None),
        (15, "other", Some("other-deal"), None),
        (
            16,
            "other",
            None,
            Some(ContractError::CancelWindowClosed {
                current: 16,
                target: 11,
                end_exclusive: 16,
            }),
        ),
    ] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate_valid(deps.as_mut());
        set_current_epoch(&mut deps, current);
        let entries = recipient
            .map(|label| ClaimRecipientEntry {
                epoch: 11,
                recipient: test_addr(label).to_string(),
            })
            .into_iter()
            .collect();
        set_claim_recipients(&mut deps, entries);
        let config_before = CONFIG.load(&deps.storage).unwrap();
        let state_before = STATE.load(&deps.storage).unwrap();
        let result = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr(caller), &[]),
            ExecuteMsg::Cancel {},
        );
        match expected {
            Some(error) => {
                assert_eq!(result.unwrap_err(), error);
                assert_eq!(STATE.load(&deps.storage).unwrap(), state_before);
            }
            None => {
                let response = result.unwrap();
                assert!(response.messages.is_empty());
                let state = STATE.load(&deps.storage).unwrap();
                assert_eq!(state.status, DealStatus::Cancelled);
                assert_eq!(state.buyer, None);
                assert!(!state.recipient_locked);
                assert_eq!(CONFIG.load(&deps.storage).unwrap(), config_before);
                assert_event_attributes(
                    &response,
                    "deal_cancelled",
                    &[
                        ("deal", mock_env().contract.address.to_string()),
                        ("host", test_addr(HOST).to_string()),
                        ("target_epoch", "11".to_string()),
                    ],
                );
            }
        }
    }
}

#[test]
fn cancel_rejects_exact_invalid_duplicate_and_failed_routing_proofs() {
    let deal = mock_env().contract.address.to_string();
    let responses = [
        (
            Ok(Binary::from(
                QueryListClaimRecipientsResponse {
                    entries: vec![ClaimRecipientEntry {
                        epoch: 11,
                        recipient: deal.clone(),
                    }],
                }
                .encode_to_vec(),
            )),
            ContractError::ClaimRecipientStillRouted { epoch: 11 },
        ),
        (
            Ok(Binary::from(
                QueryListClaimRecipientsResponse {
                    entries: vec![ClaimRecipientEntry {
                        epoch: 11,
                        recipient: "x".to_string(),
                    }],
                }
                .encode_to_vec(),
            )),
            ContractError::Gonka(GonkaQueryError::InvalidAddress {
                route: crate::gonka::LIST_CLAIM_RECIPIENTS_PATH,
                field: "recipient",
            }),
        ),
        (
            Ok(Binary::from(
                QueryListClaimRecipientsResponse {
                    entries: vec![
                        ClaimRecipientEntry {
                            epoch: 11,
                            recipient: deal.clone(),
                        },
                        ClaimRecipientEntry {
                            epoch: 11,
                            recipient: test_addr("other").to_string(),
                        },
                    ],
                }
                .encode_to_vec(),
            )),
            ContractError::Gonka(GonkaQueryError::DuplicateClaimRecipient { epoch: 11 }),
        ),
    ];
    for (response, expected) in responses {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate_valid(deps.as_mut());
        set_current_epoch(&mut deps, 11);
        deps.querier.claim_recipients_response = response;
        let before = STATE.load(&deps.storage).unwrap();
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("caller"), &[]),
                ExecuteMsg::Cancel {},
            )
            .unwrap_err(),
            expected
        );
        assert_eq!(STATE.load(&deps.storage).unwrap(), before);
    }

    for response in [
        Err(()),
        Ok(Binary::from(vec![0xff])),
        Ok(Binary::from(vec![
            0;
            crate::gonka::MAX_GRPC_RESPONSE_BYTES + 1
        ])),
    ] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate_valid(deps.as_mut());
        set_current_epoch(&mut deps, 11);
        deps.querier.claim_recipients_response = response;
        let before = STATE.load(&deps.storage).unwrap();
        assert!(matches!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("caller"), &[]),
                ExecuteMsg::Cancel {},
            ),
            Err(ContractError::Gonka(
                GonkaQueryError::QueryFailed { .. }
                    | GonkaQueryError::DecodeFailed { .. }
                    | GonkaQueryError::ResponseTooLarge { .. }
            ))
        ));
        assert_eq!(STATE.load(&deps.storage).unwrap(), before);
    }

    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    instantiate_valid(deps.as_mut());
    set_current_epoch(&mut deps, 11);
    set_claim_recipients(
        &mut deps,
        (0..=crate::gonka::MAX_CLAIM_RECIPIENT_ENTRIES)
            .map(|epoch| ClaimRecipientEntry {
                epoch: epoch as u64,
                recipient: deal.clone(),
            })
            .collect(),
    );
    let before = STATE.load(&deps.storage).unwrap();
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("caller"), &[]),
            ExecuteMsg::Cancel {},
        )
        .unwrap_err(),
        ContractError::Gonka(GonkaQueryError::TooManyClaimRecipients {
            actual: crate::gonka::MAX_CLAIM_RECIPIENT_ENTRIES + 1,
            maximum: crate::gonka::MAX_CLAIM_RECIPIENT_ENTRIES,
        })
    );
    assert_eq!(STATE.load(&deps.storage).unwrap(), before);
}

fn prepare_funded_for_refund(
    deps: &mut OwnedDeps<MockStorage, MockApi, DealMockQuerier, Empty>,
    current_epoch: u64,
    entries: Vec<ClaimRecipientEntry>,
) {
    instantiate_valid(deps.as_mut());
    execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr(TOKEN), &[]),
        exact_fund_receive(&test_addr(BUYER)),
    )
    .unwrap();
    set_current_epoch(deps, current_epoch);
    set_claim_recipients(deps, entries);
}

#[test]
fn routing_refund_enforces_epoch_boundaries_and_freezes_exact_host_only_outcome() {
    for (current, expected_error) in [
        (
            10,
            Some(ContractError::RefundWindowNotOpen {
                current: 10,
                target: 11,
            }),
        ),
        (11, None),
        (15, None),
        (
            16,
            Some(ContractError::RefundWindowClosed {
                current: 16,
                target: 11,
                end_exclusive: 16,
            }),
        ),
    ] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        prepare_funded_for_refund(&mut deps, current, vec![]);
        let config_before = CONFIG.load(&deps.storage).unwrap();
        let state_before = STATE.load(&deps.storage).unwrap();
        let result = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("permissionless-caller"), &[]),
            ExecuteMsg::Refund {},
        );

        if let Some(expected_error) = expected_error {
            assert_eq!(result.unwrap_err(), expected_error);
            assert_eq!(STATE.load(&deps.storage).unwrap(), state_before);
            continue;
        }

        let response = result.unwrap();
        assert_eq!(response.messages.len(), 1);
        assert_cw20_transfer(
            &response.messages[0],
            &test_addr(TOKEN),
            &test_addr(BUYER),
            config_before.buyer_budget_micro_usdt,
        );
        let state = STATE.load(&deps.storage).unwrap();
        assert_eq!(state.status, DealStatus::Refunded);
        assert_eq!(state.refund_reason, Some(RefundReason::RoutingMissing));
        assert_eq!(state.buyer, Some(test_addr(BUYER)));
        assert!(!state.recipient_locked);
        assert_eq!(state.gnk_release_policy, GnkReleasePolicy::HostOnly);
        assert_eq!(state.work_ngonka, Uint128::zero());
        assert_eq!(state.reward_ngonka, Uint128::zero());
        assert_eq!(state.total_claim_ngonka, Uint128::zero());
        assert_eq!(state.buyer_entitlement_ngonka, Uint128::zero());
        assert_eq!(state.host_entitlement_ngonka, Uint128::zero());
        assert_eq!(state.released_total_ngonka, Uint256::zero());
        assert_eq!(state.buyer_released_ngonka, Uint256::zero());
        assert_eq!(state.host_released_ngonka, Uint256::zero());
        assert_eq!(state.gross_usdt, Uint128::zero());
        assert_eq!(state.fee_usdt, Uint128::zero());
        assert_eq!(state.host_net_usdt, Uint128::zero());
        assert_eq!(
            state.buyer_refund_usdt,
            config_before.buyer_budget_micro_usdt
        );
        assert_eq!(CONFIG.load(&deps.storage).unwrap(), config_before);
        assert_event_attributes(
            &response,
            "deal_refunded",
            &[
                ("deal", mock_env().contract.address.to_string()),
                ("host", test_addr(HOST).to_string()),
                ("target_epoch", "11".to_string()),
                ("buyer", test_addr(BUYER).to_string()),
                ("reason", "routing_missing".to_string()),
                ("status", "refunded".to_string()),
                (
                    "amount_micro_usdt",
                    config_before.buyer_budget_micro_usdt.to_string(),
                ),
            ],
        );
        assert_event_attributes(
            &response,
            "usdt_refunded",
            &[
                ("recipient", test_addr(BUYER).to_string()),
                ("reason", "routing_missing".to_string()),
                (
                    "amount_micro_usdt",
                    config_before.buyer_budget_micro_usdt.to_string(),
                ),
            ],
        );
        let queried: StateResponse =
            from_json(query(deps.as_ref(), mock_env(), QueryMsg::State {}).unwrap()).unwrap();
        assert_eq!(queried.status, DealStatus::Refunded);
        assert_eq!(queried.refund_reason, Some(RefundReason::RoutingMissing));
        assert_eq!(
            queried.buyer_refund_usdt,
            config_before.buyer_budget_micro_usdt
        );
    }
}

#[test]
fn routing_refund_accepts_valid_mismatch_and_rejects_exact_or_invalid_evidence() {
    let other = test_addr("other-deal");
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    prepare_funded_for_refund(
        &mut deps,
        11,
        vec![ClaimRecipientEntry {
            epoch: 11,
            recipient: other.to_string(),
        }],
    );
    let response = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr("caller"), &[]),
        ExecuteMsg::Refund {},
    )
    .unwrap();
    assert_eq!(
        STATE.load(&deps.storage).unwrap().refund_reason,
        Some(RefundReason::RoutingMismatch)
    );
    assert_event_attributes(
        &response,
        "deal_refunded",
        &[
            ("reason", "routing_mismatch".to_string()),
            ("observed_recipient", other.to_string()),
        ],
    );

    let deal = mock_env().contract.address.to_string();
    let cases = [
        (
            Ok(Binary::from(
                QueryListClaimRecipientsResponse {
                    entries: vec![ClaimRecipientEntry {
                        epoch: 11,
                        recipient: deal.clone(),
                    }],
                }
                .encode_to_vec(),
            )),
            ContractError::RefundRecipientStillRouted { epoch: 11 },
        ),
        (
            Ok(Binary::from(
                QueryListClaimRecipientsResponse {
                    entries: vec![ClaimRecipientEntry {
                        epoch: 11,
                        recipient: "x".to_string(),
                    }],
                }
                .encode_to_vec(),
            )),
            ContractError::Gonka(GonkaQueryError::InvalidAddress {
                route: crate::gonka::LIST_CLAIM_RECIPIENTS_PATH,
                field: "recipient",
            }),
        ),
        (
            Ok(Binary::from(
                QueryListClaimRecipientsResponse {
                    entries: vec![
                        ClaimRecipientEntry {
                            epoch: 11,
                            recipient: deal.clone(),
                        },
                        ClaimRecipientEntry {
                            epoch: 11,
                            recipient: other.to_string(),
                        },
                    ],
                }
                .encode_to_vec(),
            )),
            ContractError::Gonka(GonkaQueryError::DuplicateClaimRecipient { epoch: 11 }),
        ),
    ];
    for (response, expected) in cases {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        prepare_funded_for_refund(&mut deps, 11, vec![]);
        deps.querier.claim_recipients_response = response;
        let before = STATE.load(&deps.storage).unwrap();
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("caller"), &[]),
                ExecuteMsg::Refund {},
            )
            .unwrap_err(),
            expected
        );
        assert_eq!(STATE.load(&deps.storage).unwrap(), before);
    }

    for response in [
        Err(()),
        Ok(Binary::from(vec![0xff])),
        Ok(Binary::from(vec![
            0;
            crate::gonka::MAX_GRPC_RESPONSE_BYTES + 1
        ])),
    ] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        prepare_funded_for_refund(&mut deps, 11, vec![]);
        deps.querier.claim_recipients_response = response;
        let before = STATE.load(&deps.storage).unwrap();
        assert!(matches!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("caller"), &[]),
                ExecuteMsg::Refund {},
            ),
            Err(ContractError::Gonka(
                GonkaQueryError::QueryFailed { .. }
                    | GonkaQueryError::DecodeFailed { .. }
                    | GonkaQueryError::ResponseTooLarge { .. }
            ))
        ));
        assert_eq!(STATE.load(&deps.storage).unwrap(), before);
    }
}

#[test]
fn claim_expiry_enforces_e_plus_two_and_handles_funded_or_no_sale_summaries() {
    for current in [11, 12] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        prepare_locked(&mut deps, true);
        set_current_epoch(&mut deps, current);
        set_performance(&mut deps, 11, test_addr(HOST).to_string(), 0, 0, false);
        let before = STATE.load(&deps.storage).unwrap();
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("caller"), &[]),
                ExecuteMsg::Refund {},
            )
            .unwrap_err(),
            ContractError::ClaimExpiryWindowNotOpen {
                current,
                target: 11,
                first_allowed: 13,
            }
        );
        assert_eq!(STATE.load(&deps.storage).unwrap(), before);
    }

    for (funded, current, earned, rewarded) in [
        (true, 13, 0, 0),
        (true, 99, 1, 2),
        (false, 13, 0, 0),
        (false, 99, 1, 2),
    ] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        prepare_locked(&mut deps, funded);
        set_current_epoch(&mut deps, current);
        set_performance(
            &mut deps,
            11,
            test_addr(HOST).to_string(),
            earned,
            rewarded,
            false,
        );
        let config_before = CONFIG.load(&deps.storage).unwrap();
        let response = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("permissionless-caller"), &[]),
            ExecuteMsg::Refund {},
        )
        .unwrap();
        let state = STATE.load(&deps.storage).unwrap();

        assert_eq!(state.refund_reason, Some(RefundReason::ClaimExpiry));
        assert!(state.recipient_locked);
        assert_eq!(state.gnk_release_policy, GnkReleasePolicy::HostOnly);
        assert_eq!(state.work_ngonka, Uint128::zero());
        assert_eq!(state.reward_ngonka, Uint128::zero());
        assert_eq!(state.total_claim_ngonka, Uint128::zero());
        assert_eq!(state.gross_usdt, Uint128::zero());
        assert_eq!(state.fee_usdt, Uint128::zero());
        assert_eq!(state.host_net_usdt, Uint128::zero());
        assert_eq!(CONFIG.load(&deps.storage).unwrap(), config_before);

        if funded {
            assert_eq!(state.status, DealStatus::Refunded);
            assert_eq!(state.buyer, Some(test_addr(BUYER)));
            assert_eq!(
                state.buyer_refund_usdt,
                config_before.buyer_budget_micro_usdt
            );
            assert_eq!(response.messages.len(), 1);
            assert_cw20_transfer(
                &response.messages[0],
                &test_addr(TOKEN),
                &test_addr(BUYER),
                config_before.buyer_budget_micro_usdt,
            );
            assert_event_attributes(
                &response,
                "deal_refunded",
                &[
                    ("reason", "claim_expiry".to_string()),
                    ("status", "refunded".to_string()),
                ],
            );
            assert_event_attributes(
                &response,
                "usdt_refunded",
                &[("reason", "claim_expiry".to_string())],
            );
        } else {
            assert_eq!(state.status, DealStatus::Expired);
            assert_eq!(state.buyer, None);
            assert_eq!(state.buyer_refund_usdt, Uint128::zero());
            assert!(response.messages.is_empty());
            assert_event_attributes(
                &response,
                "deal_expired",
                &[
                    ("reason", "claim_expiry".to_string()),
                    ("status", "expired".to_string()),
                ],
            );
        }
    }
}

#[test]
fn claim_expiry_rejects_claimed_or_untrustworthy_summary_evidence() {
    for (current, earned, rewarded) in [(13, 0, 0), (14, 0, 0), (99, 1, 2)] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        prepare_locked(&mut deps, true);
        set_current_epoch(&mut deps, current);
        set_performance(
            &mut deps,
            11,
            test_addr(HOST).to_string(),
            earned,
            rewarded,
            true,
        );
        let before = STATE.load(&deps.storage).unwrap();
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("caller"), &[]),
                ExecuteMsg::Refund {},
            )
            .unwrap_err(),
            ContractError::ClaimAlreadyConfirmed { epoch: 11 }
        );
        assert_eq!(STATE.load(&deps.storage).unwrap(), before);
    }

    for fault in [
        SummaryFault::MissingNestedSummary,
        SummaryFault::MalformedProtobuf,
        SummaryFault::OversizedResponse,
        SummaryFault::UnsupportedRequest,
        SummaryFault::WrongEpoch,
        SummaryFault::WrongHost,
    ] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        prepare_locked(&mut deps, true);
        set_current_epoch(&mut deps, 13);
        let (_, expected) = inject_summary_fault(&mut deps, fault);
        let before = storage_snapshot(&deps.storage);
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("caller"), &[]),
                ExecuteMsg::Refund {},
            )
            .unwrap_err(),
            ContractError::Gonka(expected)
        );
        assert_eq!(storage_snapshot(&deps.storage), before);
    }
}

#[test]
fn network_unconfirmed_refund_accepts_only_explicit_summary_failure_matrix_after_e_plus_three() {
    for (index, case) in [
        SummaryFault::HandlerError,
        SummaryFault::UnsupportedRequest,
        SummaryFault::InvalidResponse,
        SummaryFault::MissingNestedSummary,
        SummaryFault::MalformedProtobuf,
        SummaryFault::OversizedResponse,
        SummaryFault::WrongEpoch,
        SummaryFault::InvalidParticipantAddress,
        SummaryFault::WrongHost,
    ]
    .into_iter()
    .enumerate()
    {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        prepare_locked(&mut deps, true);
        set_current_epoch(&mut deps, if index % 2 == 0 { 14 } else { 99 });
        inject_summary_fault(&mut deps, case);
        let before = STATE.load(&deps.storage).unwrap();

        let response = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("permissionless-caller"), &[]),
            ExecuteMsg::Refund {},
        )
        .unwrap();
        assert_network_unconfirmed_refund(&deps.storage, &response, before);
    }
}

#[test]
fn network_unconfirmed_refund_rejects_early_and_unexpected_system_failures() {
    let refundable_error = SystemResult::Ok(ContractResult::Err("native error".to_string()));
    let mut early = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    prepare_locked(&mut early, true);
    set_current_epoch(&mut early, 13);
    early.querier.performance_result_override = Some(refundable_error);
    let before = STATE.load(&early.storage).unwrap();
    assert_eq!(
        execute(
            early.as_mut(),
            mock_env(),
            message_info(&test_addr("caller"), &[]),
            ExecuteMsg::Refund {},
        )
        .unwrap_err(),
        ContractError::Gonka(GonkaQueryError::QueryFailed {
            route: crate::gonka::EPOCH_PERFORMANCE_PATH,
        })
    );
    assert_eq!(STATE.load(&early.storage).unwrap(), before);

    let system_errors = [
        SystemError::InvalidRequest {
            error: "request parse".to_string(),
            request: Binary::default(),
        },
        SystemError::NoSuchContract {
            addr: "irrelevant".to_string(),
        },
        SystemError::NoSuchCode { code_id: 7 },
        SystemError::Unknown {},
    ];
    for system_error in system_errors {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        prepare_locked(&mut deps, true);
        set_current_epoch(&mut deps, 14);
        deps.querier.performance_result_override = Some(SystemResult::Err(system_error));
        let before = STATE.load(&deps.storage).unwrap();
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("caller"), &[]),
                ExecuteMsg::Refund {},
            )
            .unwrap_err(),
            ContractError::Gonka(GonkaQueryError::UnexpectedSystemError {
                route: crate::gonka::EPOCH_PERFORMANCE_PATH,
            })
        );
        assert_eq!(STATE.load(&deps.storage).unwrap(), before);
    }

    for error in [
        GonkaQueryError::EncodeFailed {
            route: crate::gonka::EPOCH_PERFORMANCE_PATH,
        },
        GonkaQueryError::ArithmeticOverflow {
            operation: "test request arithmetic",
        },
        GonkaQueryError::UnexpectedSystemError {
            route: crate::gonka::EPOCH_PERFORMANCE_PATH,
        },
    ] {
        assert!(!error.permits_network_unconfirmed_refund());
    }
}

#[test]
fn refund_rechecks_current_summary_and_current_epoch_on_every_attempt() {
    for recovered_claimed in [true, false] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        prepare_locked(&mut deps, true);
        set_current_epoch(&mut deps, 13);
        deps.querier.performance_result_override = Some(SystemResult::Ok(ContractResult::Err(
            "temporary native error".to_string(),
        )));
        assert!(matches!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("caller"), &[]),
                ExecuteMsg::Refund {},
            ),
            Err(ContractError::Gonka(GonkaQueryError::QueryFailed { .. }))
        ));

        set_current_epoch(&mut deps, 14);
        deps.querier.performance_result_override = None;
        set_performance(
            &mut deps,
            11,
            test_addr(HOST).to_string(),
            0,
            0,
            recovered_claimed,
        );
        let result = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("caller"), &[]),
            ExecuteMsg::Refund {},
        );
        if recovered_claimed {
            assert_eq!(
                result.unwrap_err(),
                ContractError::ClaimAlreadyConfirmed { epoch: 11 }
            );
            assert_eq!(
                STATE.load(&deps.storage).unwrap().status,
                DealStatus::Locked
            );
        } else {
            result.unwrap();
            let state = STATE.load(&deps.storage).unwrap();
            assert_eq!(state.status, DealStatus::Refunded);
            assert_eq!(state.refund_reason, Some(RefundReason::ClaimExpiry));
        }
    }

    for current_epoch_response in [
        Err(()),
        Ok(Binary::from(vec![0xff])),
        Ok(Binary::from(vec![
            0;
            marketplace_common::gonka::MAX_GRPC_RESPONSE_BYTES
                + 1
        ])),
    ] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        prepare_locked(&mut deps, true);
        deps.querier.current_epoch_response = current_epoch_response;
        deps.querier.performance_result_override = Some(SystemResult::Ok(ContractResult::Err(
            "summary is also unavailable".to_string(),
        )));
        let before = STATE.load(&deps.storage).unwrap();
        assert!(matches!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("caller"), &[]),
                ExecuteMsg::Refund {},
            ),
            Err(ContractError::CommonGonka(_))
        ));
        assert_eq!(STATE.load(&deps.storage).unwrap(), before);
    }
}

#[test]
fn claim_expiry_checks_pristine_locked_accounting_and_epoch_overflow() {
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    prepare_locked(&mut deps, true);
    set_current_epoch(&mut deps, 13);
    let mut corrupted = STATE.load(&deps.storage).unwrap();
    corrupted.host_net_usdt = Uint128::one();
    STATE.save(&mut deps.storage, &corrupted).unwrap();
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("caller"), &[]),
            ExecuteMsg::Refund {},
        )
        .unwrap_err(),
        ContractError::InconsistentRefundAccounting
    );
    assert_eq!(STATE.load(&deps.storage).unwrap(), corrupted);

    for target in [u64::MAX - 2, u64::MAX - 1] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        prepare_locked(&mut deps, false);
        set_current_epoch(&mut deps, u64::MAX);
        CONFIG
            .update(&mut deps.storage, |mut config| -> StdResult<_> {
                config.target_epoch = target;
                Ok(config)
            })
            .unwrap();
        let before = STATE.load(&deps.storage).unwrap();
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("caller"), &[]),
                ExecuteMsg::Refund {},
            )
            .unwrap_err(),
            ContractError::ClaimExpiryEpochOverflow { target }
        );
        assert_eq!(STATE.load(&deps.storage).unwrap(), before);
    }
}

#[test]
fn refund_does_not_reopen_non_refundable_states() {
    for status in [
        DealStatus::Open,
        DealStatus::Releasing,
        DealStatus::Completed,
        DealStatus::Refunded,
        DealStatus::Cancelled,
        DealStatus::Expired,
    ] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate_valid(deps.as_mut());
        let mut state = DealState::open();
        state.status = status.clone();
        STATE.save(&mut deps.storage, &state).unwrap();
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("caller"), &[]),
                ExecuteMsg::Refund {},
            )
            .unwrap_err(),
            ContractError::InvalidRefundState { actual: status }
        );
        assert_eq!(STATE.load(&deps.storage).unwrap(), state);
    }
}

#[test]
fn routing_refund_checks_internal_buyer_and_epoch_overflow_without_mutation() {
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    instantiate_valid(deps.as_mut());
    let mut corrupted = DealState::open();
    corrupted.status = DealStatus::Funded;
    STATE.save(&mut deps.storage, &corrupted).unwrap();
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("caller"), &[]),
            ExecuteMsg::Refund {},
        )
        .unwrap_err(),
        ContractError::RefundWithoutBuyer
    );
    assert_eq!(STATE.load(&deps.storage).unwrap(), corrupted);

    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    prepare_funded_for_refund(&mut deps, 11, vec![]);
    let mut corrupted = STATE.load(&deps.storage).unwrap();
    corrupted.released_total_ngonka = Uint256::one();
    STATE.save(&mut deps.storage, &corrupted).unwrap();
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("caller"), &[]),
            ExecuteMsg::Refund {},
        )
        .unwrap_err(),
        ContractError::InconsistentRefundAccounting
    );
    assert_eq!(STATE.load(&deps.storage).unwrap(), corrupted);

    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    prepare_funded_for_refund(&mut deps, u64::MAX - 4, vec![]);
    CONFIG
        .update(&mut deps.storage, |mut config| -> StdResult<_> {
            config.target_epoch = u64::MAX - 4;
            Ok(config)
        })
        .unwrap();
    let before = STATE.load(&deps.storage).unwrap();
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("caller"), &[]),
            ExecuteMsg::Refund {},
        )
        .unwrap_err(),
        ContractError::RoutingWindowOverflow {
            target: u64::MAX - 4,
        }
    );
    assert_eq!(STATE.load(&deps.storage).unwrap(), before);
}

#[test]
fn lock_and_cancel_fail_closed_on_current_epoch_query_errors() {
    for response in [
        Err(()),
        Ok(Binary::from(vec![0xff])),
        Ok(Binary::from(vec![
            0;
            marketplace_common::gonka::MAX_GRPC_RESPONSE_BYTES
                + 1
        ])),
    ] {
        for msg in [ExecuteMsg::Lock {}, ExecuteMsg::Cancel {}] {
            let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
            instantiate_valid(deps.as_mut());
            deps.querier.current_epoch_response = response.clone();
            let before = STATE.load(&deps.storage).unwrap();
            assert!(matches!(
                execute(
                    deps.as_mut(),
                    mock_env(),
                    message_info(&test_addr(HOST), &[]),
                    msg,
                ),
                Err(ContractError::CommonGonka(_))
            ));
            assert_eq!(STATE.load(&deps.storage).unwrap(), before);
        }
    }
}

#[test]
fn lock_cancel_wrong_state_repeat_native_funds_and_overflow_leave_state_unchanged() {
    for status in [
        DealStatus::Locked,
        DealStatus::Releasing,
        DealStatus::Completed,
        DealStatus::Refunded,
        DealStatus::Cancelled,
        DealStatus::Expired,
    ] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate_valid(deps.as_mut());
        let mut state = DealState::open();
        state.status = status.clone();
        STATE.save(&mut deps.storage, &state).unwrap();
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("caller"), &[]),
                ExecuteMsg::Lock {},
            )
            .unwrap_err(),
            ContractError::InvalidLockState {
                actual: status.clone(),
            }
        );
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("caller"), &[]),
                ExecuteMsg::Cancel {},
            )
            .unwrap_err(),
            ContractError::InvalidCancelState { actual: status }
        );
        assert_eq!(STATE.load(&deps.storage).unwrap(), state);
    }

    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    instantiate_valid(deps.as_mut());
    execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr(TOKEN), &[]),
        exact_fund_receive(&test_addr(BUYER)),
    )
    .unwrap();
    let funded = STATE.load(&deps.storage).unwrap();
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr(HOST), &[]),
            ExecuteMsg::Cancel {},
        )
        .unwrap_err(),
        ContractError::InvalidCancelState {
            actual: DealStatus::Funded,
        }
    );
    assert_eq!(STATE.load(&deps.storage).unwrap(), funded);

    for msg in [ExecuteMsg::Lock {}, ExecuteMsg::Cancel {}] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate_valid(deps.as_mut());
        let before = STATE.load(&deps.storage).unwrap();
        assert!(matches!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr(HOST), &[coin(1, "ngonka")]),
                msg,
            ),
            Err(ContractError::Payment(_))
        ));
        assert_eq!(STATE.load(&deps.storage).unwrap(), before);
    }

    for msg in [ExecuteMsg::Lock {}, ExecuteMsg::Cancel {}] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate_valid(deps.as_mut());
        CONFIG
            .update(&mut deps.storage, |mut config| -> StdResult<_> {
                config.target_epoch = u64::MAX - 4;
                Ok(config)
            })
            .unwrap();
        set_current_epoch(&mut deps, u64::MAX - 5);
        let before = STATE.load(&deps.storage).unwrap();
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr(HOST), &[]),
                msg,
            )
            .unwrap_err(),
            ContractError::RoutingWindowOverflow {
                target: u64::MAX - 4,
            }
        );
        assert_eq!(STATE.load(&deps.storage).unwrap(), before);
    }
}

#[test]
fn exact_funding_records_buyer_and_funded_without_outgoing_messages() {
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    instantiate_valid(deps.as_mut());
    let config_before = CONFIG.load(&deps.storage).unwrap();
    let buyer = test_addr(BUYER);

    let response = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr(TOKEN), &[]),
        exact_fund_receive(&buyer),
    )
    .unwrap();

    assert!(response.messages.is_empty());
    assert_eq!(CONFIG.load(&deps.storage).unwrap(), config_before);
    let mut expected = DealState::open();
    expected.status = DealStatus::Funded;
    expected.buyer = Some(buyer.clone());
    assert_eq!(STATE.load(&deps.storage).unwrap(), expected);

    let event = response
        .events
        .iter()
        .find(|event| event.ty == "deal_funded")
        .unwrap();
    for (key, value) in [
        ("deal", mock_env().contract.address.to_string()),
        ("host", test_addr(HOST).to_string()),
        ("target_epoch", "11".to_string()),
        ("buyer", buyer.to_string()),
        ("buyer_budget_micro_usdt", "100000000".to_string()),
    ] {
        assert!(event
            .attributes
            .iter()
            .any(|attribute| attribute.key == key && attribute.value == value));
    }

    let funding: FundingResponse =
        from_json(query(deps.as_ref(), mock_env(), QueryMsg::Funding {}).unwrap()).unwrap();
    assert!(funding.funded);
    assert_eq!(funding.buyer, Some(buyer.to_string()));
    assert_eq!(
        funding.buyer_budget_micro_usdt,
        config_before.buyer_budget_micro_usdt
    );
    assert_eq!(
        funding.funded_capacity_ngonka,
        config_before.funded_capacity_ngonka
    );
}

#[test]
fn receive_rejects_untrusted_caller_malformed_hook_invalid_buyer_and_native_funds() {
    let hook = to_json_binary(&Cw20HookMsg::Fund {}).unwrap();
    let original = DealState::open();

    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    instantiate_valid(deps.as_mut());
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr(BUYER), &[]),
            fund_receive(test_addr(BUYER).as_str(), 100_000_000, hook.clone()),
        )
        .unwrap_err(),
        ContractError::WrongCw20
    );

    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("wrong-token"), &[]),
            fund_receive(
                test_addr(BUYER).as_str(),
                100_000_000,
                Binary::from(b"not-json")
            ),
        )
        .unwrap_err(),
        ContractError::WrongCw20
    );

    assert!(matches!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr(TOKEN), &[]),
            fund_receive(
                test_addr(BUYER).as_str(),
                100_000_000,
                Binary::from(b"not-json")
            ),
        ),
        Err(ContractError::Std(_))
    ));

    assert!(matches!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr(TOKEN), &[]),
            fund_receive("x", 100_000_000, hook.clone()),
        ),
        Err(ContractError::Std(_))
    ));

    assert!(matches!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr(TOKEN), &[coin(1, "ngonka")]),
            fund_receive(test_addr(BUYER).as_str(), 100_000_000, hook),
        ),
        Err(ContractError::Payment(_))
    ));
    assert_eq!(STATE.load(&deps.storage).unwrap(), original);
}

#[test]
fn funding_requires_exact_budget_and_open_state_without_replacing_buyer() {
    for amount in [99_999_999, 100_000_001] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate_valid(deps.as_mut());
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr(TOKEN), &[]),
                fund_receive(
                    test_addr(BUYER).as_str(),
                    amount,
                    to_json_binary(&Cw20HookMsg::Fund {}).unwrap(),
                ),
            )
            .unwrap_err(),
            ContractError::WrongFundingAmount {
                expected: Uint128::new(100_000_000),
                actual: Uint128::new(amount),
            }
        );
        assert_eq!(STATE.load(&deps.storage).unwrap(), DealState::open());
    }

    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    instantiate_valid(deps.as_mut());
    let first_buyer = test_addr(BUYER);
    execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr(TOKEN), &[]),
        exact_fund_receive(&first_buyer),
    )
    .unwrap();
    let funded = STATE.load(&deps.storage).unwrap();
    for buyer in [first_buyer, test_addr("buyer-two")] {
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr(TOKEN), &[]),
                exact_fund_receive(&buyer),
            )
            .unwrap_err(),
            ContractError::InvalidFundingState {
                actual: DealStatus::Funded,
            }
        );
        assert_eq!(STATE.load(&deps.storage).unwrap(), funded);
    }

    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    instantiate_valid(deps.as_mut());
    let mut locked = DealState::open();
    locked.status = DealStatus::Locked;
    STATE.save(&mut deps.storage, &locked).unwrap();
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr(TOKEN), &[]),
            exact_fund_receive(&test_addr(BUYER)),
        )
        .unwrap_err(),
        ContractError::InvalidFundingState {
            actual: DealStatus::Locked,
        }
    );
    assert_eq!(STATE.load(&deps.storage).unwrap(), locked);
}

#[test]
fn funding_closes_at_target_epoch_and_fails_closed_on_epoch_query_errors() {
    for current in [11, 12] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate_valid(deps.as_mut());
        deps.querier.current_epoch_response = Ok(Binary::from(
            QueryGetCurrentEpochResponse { epoch: current }.encode_to_vec(),
        ));
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr(TOKEN), &[]),
                exact_fund_receive(&test_addr(BUYER)),
            )
            .unwrap_err(),
            ContractError::LateFunding {
                current,
                target: 11,
            }
        );
        assert_eq!(STATE.load(&deps.storage).unwrap(), DealState::open());
    }

    for response in [Err(()), Ok(Binary::from(vec![0xff]))] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate_valid(deps.as_mut());
        deps.querier.current_epoch_response = response;
        assert!(matches!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr(TOKEN), &[]),
                exact_fund_receive(&test_addr(BUYER)),
            ),
            Err(ContractError::CommonGonka(_))
        ));
        assert_eq!(STATE.load(&deps.storage).unwrap(), DealState::open());
    }
}

#[test]
fn funding_requires_one_valid_exact_host_epoch_recipient() {
    let deal = mock_env().contract.address.to_string();
    let other = test_addr("other-deal").to_string();
    let cases = [
        (
            Ok(Binary::from(
                QueryListClaimRecipientsResponse { entries: vec![] }.encode_to_vec(),
            )),
            GonkaQueryError::ClaimRecipientNotFound { epoch: 11 },
        ),
        (
            Ok(Binary::from(
                QueryListClaimRecipientsResponse {
                    entries: vec![ClaimRecipientEntry {
                        epoch: 11,
                        recipient: other,
                    }],
                }
                .encode_to_vec(),
            )),
            GonkaQueryError::IdentityMismatch {
                route: crate::gonka::LIST_CLAIM_RECIPIENTS_PATH,
                field: "recipient",
            },
        ),
        (
            Ok(Binary::from(
                QueryListClaimRecipientsResponse {
                    entries: vec![ClaimRecipientEntry {
                        epoch: 11,
                        recipient: "x".to_string(),
                    }],
                }
                .encode_to_vec(),
            )),
            GonkaQueryError::InvalidAddress {
                route: crate::gonka::LIST_CLAIM_RECIPIENTS_PATH,
                field: "recipient",
            },
        ),
        (
            Ok(Binary::from(
                QueryListClaimRecipientsResponse {
                    entries: vec![
                        ClaimRecipientEntry {
                            epoch: 11,
                            recipient: deal.clone(),
                        },
                        ClaimRecipientEntry {
                            epoch: 11,
                            recipient: deal,
                        },
                    ],
                }
                .encode_to_vec(),
            )),
            GonkaQueryError::DuplicateClaimRecipient { epoch: 11 },
        ),
    ];

    for (response, expected) in cases {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate_valid(deps.as_mut());
        deps.querier.claim_recipients_response = response;
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr(TOKEN), &[]),
                exact_fund_receive(&test_addr(BUYER)),
            )
            .unwrap_err(),
            ContractError::Gonka(expected)
        );
        assert_eq!(STATE.load(&deps.storage).unwrap(), DealState::open());
    }

    for response in [Err(()), Ok(Binary::from(vec![0xff]))] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate_valid(deps.as_mut());
        deps.querier.claim_recipients_response = response;
        assert!(matches!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr(TOKEN), &[]),
                exact_fund_receive(&test_addr(BUYER)),
            ),
            Err(ContractError::Gonka(
                GonkaQueryError::QueryFailed { .. } | GonkaQueryError::DecodeFailed { .. }
            ))
        ));
        assert_eq!(STATE.load(&deps.storage).unwrap(), DealState::open());
    }
}

#[test]
fn derived_queries_report_initial_values() {
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    instantiate_valid(deps.as_mut());

    let funding: FundingResponse =
        from_json(query(deps.as_ref(), mock_env(), QueryMsg::Funding {}).unwrap()).unwrap();
    assert!(!funding.funded);
    assert_eq!(funding.buyer, None);

    let entitlements: EntitlementsResponse =
        from_json(query(deps.as_ref(), mock_env(), QueryMsg::Entitlements {}).unwrap()).unwrap();
    assert_eq!(entitlements.total_claim_ngonka, Uint128::zero());
    assert_eq!(entitlements.gnk_release_policy, GnkReleasePolicy::Unset);

    let release: ReleaseStatusResponse =
        from_json(query(deps.as_ref(), mock_env(), QueryMsg::ReleaseStatus {}).unwrap()).unwrap();
    assert_eq!(release.released_total_ngonka, Uint256::zero());
    assert_eq!(release.buyer_original_remaining_ngonka, Uint128::zero());
    assert_eq!(release.host_original_remaining_ngonka, Uint128::zero());
    assert_eq!(release.gnk_release_policy, GnkReleasePolicy::Unset);
}

#[test]
fn release_uses_permanent_80_20_shares_crosses_completion_and_continues() {
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    instantiate_valid(deps.as_mut());
    save_releasing_state(&mut deps, Some(test_addr(BUYER)), 80);
    deps.querier
        .base
        .bank
        .update_balance(mock_env().contract.address, vec![coin(25, NGONKA_DENOM)]);

    let first = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr("permissionless-keeper"), &[]),
        ExecuteMsg::ReleaseUnlockedGnk {},
    )
    .unwrap();
    assert_eq!(first.messages.len(), 2);
    assert_bank_send(&first.messages[0], &test_addr(BUYER), Uint128::new(20));
    assert_bank_send(&first.messages[1], &test_addr(HOST), Uint128::new(5));
    assert_event_attributes(
        &first,
        "gnk_released",
        &[
            ("released_total_ngonka", "25".to_string()),
            ("buyer_released_ngonka", "20".to_string()),
            ("host_released_ngonka", "5".to_string()),
            ("buyer_delta_ngonka", "20".to_string()),
            ("host_delta_ngonka", "5".to_string()),
            ("buyer_share_numerator", "80".to_string()),
            ("host_share_numerator", "20".to_string()),
            ("share_denominator", "100".to_string()),
            ("available_balance_ngonka", "25".to_string()),
        ],
    );
    assert!(first
        .events
        .iter()
        .flat_map(|event| &event.attributes)
        .all(|attribute| attribute.key != "remaining_vesting_ngonka"));
    let first_state = STATE.load(&deps.storage).unwrap();
    assert_eq!(first_state.status, DealStatus::Releasing);
    assert_eq!(first_state.released_total_ngonka, Uint256::from(25_u128));

    deps.querier
        .base
        .bank
        .update_balance(mock_env().contract.address, vec![coin(125, NGONKA_DENOM)]);
    let crossing_release = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr("another-keeper"), &[]),
        ExecuteMsg::ReleaseUnlockedGnk {},
    )
    .unwrap();
    assert_eq!(crossing_release.messages.len(), 2);
    assert_bank_send(
        &crossing_release.messages[0],
        &test_addr(BUYER),
        Uint128::new(100),
    );
    assert_bank_send(
        &crossing_release.messages[1],
        &test_addr(HOST),
        Uint128::new(25),
    );
    assert_event_attributes(
        &crossing_release,
        "deal_completed",
        &[("total_claim_ngonka", "100".to_string())],
    );
    let completed = STATE.load(&deps.storage).unwrap();
    assert_eq!(completed.status, DealStatus::Completed);
    assert_eq!(completed.released_total_ngonka, Uint256::from(150_u128));
    assert_eq!(completed.buyer_released_ngonka, Uint256::from(120_u128));
    assert_eq!(completed.host_released_ngonka, Uint256::from(30_u128));

    deps.querier
        .base
        .bank
        .update_balance(mock_env().contract.address, vec![coin(10, NGONKA_DENOM)]);
    let late = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr("late-keeper"), &[]),
        ExecuteMsg::ReleaseUnlockedGnk {},
    )
    .unwrap();
    assert_bank_send(&late.messages[0], &test_addr(BUYER), Uint128::new(8));
    assert_bank_send(&late.messages[1], &test_addr(HOST), Uint128::new(2));
    assert!(late.events.iter().all(|event| event.ty != "deal_completed"));
    let late_state = STATE.load(&deps.storage).unwrap();
    assert_eq!(late_state.status, DealStatus::Completed);
    assert_eq!(late_state.released_total_ngonka, Uint256::from(160_u128));
    assert_eq!(late_state.buyer_released_ngonka, Uint256::from(128_u128));
    assert_eq!(late_state.host_released_ngonka, Uint256::from(32_u128));
}

#[test]
fn release_suppresses_zero_side_transfers_and_supports_no_sale() {
    for (buyer, buyer_entitlement, expected_recipient) in [
        (Some(test_addr(BUYER)), 100, test_addr(BUYER)),
        (None, 0, test_addr(HOST)),
    ] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate_valid(deps.as_mut());
        save_releasing_state(&mut deps, buyer, buyer_entitlement);
        deps.querier.remaining_vesting = Uint128::zero();
        deps.querier
            .base
            .bank
            .update_balance(mock_env().contract.address, vec![coin(100, NGONKA_DENOM)]);

        let response = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("keeper"), &[]),
            ExecuteMsg::ReleaseUnlockedGnk {},
        )
        .unwrap();
        assert_eq!(response.messages.len(), 1);
        assert_bank_send(
            &response.messages[0],
            &expected_recipient,
            Uint128::new(100),
        );
        assert_eq!(
            STATE.load(&deps.storage).unwrap().status,
            DealStatus::Completed
        );
    }
}

#[test]
fn release_ignores_vesting_query_failure_and_zero_balance_is_a_noop() {
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    instantiate_valid(deps.as_mut());
    save_releasing_state(&mut deps, Some(test_addr(BUYER)), 60);
    let mut state = STATE.load(&deps.storage).unwrap();
    state.released_total_ngonka = Uint256::from(50_u128);
    state.buyer_released_ngonka = Uint256::from(30_u128);
    state.host_released_ngonka = Uint256::from(20_u128);
    STATE.save(deps.as_mut().storage, &state).unwrap();
    deps.querier.remaining_vesting = Uint128::MAX;
    deps.querier.total_vesting_query_fails = true;
    deps.querier
        .base
        .bank
        .update_balance(mock_env().contract.address, vec![coin(10, NGONKA_DENOM)]);

    let response = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr("keeper"), &[]),
        ExecuteMsg::ReleaseUnlockedGnk {},
    )
    .unwrap();
    assert_bank_send(&response.messages[0], &test_addr(BUYER), Uint128::new(6));
    assert_bank_send(&response.messages[1], &test_addr(HOST), Uint128::new(4));
    let after_release = STATE.load(&deps.storage).unwrap();
    assert_eq!(after_release.released_total_ngonka, Uint256::from(60_u128));

    deps.querier
        .base
        .bank
        .update_balance(mock_env().contract.address, vec![]);
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("keeper"), &[]),
            ExecuteMsg::ReleaseUnlockedGnk {},
        )
        .unwrap_err(),
        ContractError::NothingToRelease
    );
    assert_eq!(STATE.load(&deps.storage).unwrap(), after_release);
}

#[test]
fn release_rejects_missing_buyer_and_inconsistent_frozen_accounting() {
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    instantiate_valid(deps.as_mut());
    save_releasing_state(&mut deps, None, 60);
    deps.querier
        .base
        .bank
        .update_balance(mock_env().contract.address, vec![coin(100, NGONKA_DENOM)]);
    let original = STATE.load(&deps.storage).unwrap();
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("keeper"), &[]),
            ExecuteMsg::ReleaseUnlockedGnk {},
        )
        .unwrap_err(),
        ContractError::BuyerReleaseWithoutBuyer
    );
    assert_eq!(STATE.load(&deps.storage).unwrap(), original);

    let mut cases = Vec::new();
    let mut bad_claim = original.clone();
    bad_claim.work_ngonka = Uint128::new(41);
    cases.push((
        bad_claim,
        ContractError::InconsistentClaimAccounting {
            work_ngonka: Uint128::new(41),
            reward_ngonka: Uint128::new(60),
            total_claim_ngonka: Uint128::new(100),
        },
    ));
    let mut bad_entitlements = original.clone();
    bad_entitlements.host_entitlement_ngonka = Uint128::new(41);
    cases.push((
        bad_entitlements,
        ContractError::InconsistentEntitlementAccounting {
            buyer_entitlement_ngonka: Uint128::new(60),
            host_entitlement_ngonka: Uint128::new(41),
            total_claim_ngonka: Uint128::new(100),
        },
    ));
    let mut bad_total = original.clone();
    bad_total.released_total_ngonka = Uint256::one();
    cases.push((
        bad_total,
        ContractError::InconsistentReleaseAccounting {
            buyer_released_ngonka: Uint256::zero(),
            host_released_ngonka: Uint256::zero(),
            released_total_ngonka: Uint256::one(),
        },
    ));
    let mut bad_policy = original.clone();
    bad_policy.gnk_release_policy = GnkReleasePolicy::Proportional {
        buyer_share_numerator: Uint128::new(59),
        share_denominator: Uint128::new(100),
    };
    cases.push((bad_policy, ContractError::InconsistentReleasePolicy));
    let mut premature_completed = original.clone();
    premature_completed.status = DealStatus::Completed;
    cases.push((
        premature_completed,
        ContractError::InconsistentReleasePolicy,
    ));
    for (bad_state, expected) in cases {
        STATE.save(deps.as_mut().storage, &bad_state).unwrap();
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("keeper"), &[]),
                ExecuteMsg::ReleaseUnlockedGnk {},
            )
            .unwrap_err(),
            expected
        );
        assert_eq!(STATE.load(&deps.storage).unwrap(), bad_state);
    }

    let mut noncanonical = original;
    noncanonical.released_total_ngonka = Uint256::from(3_u128);
    noncanonical.buyer_released_ngonka = Uint256::from(2_u128);
    noncanonical.host_released_ngonka = Uint256::one();
    STATE.save(deps.as_mut().storage, &noncanonical).unwrap();
    assert!(matches!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("keeper"), &[]),
            ExecuteMsg::ReleaseUnlockedGnk {},
        ),
        Err(ContractError::Math(
            marketplace_common::error::MathError::NonCanonicalReleaseCounters { .. }
        ))
    ));
    assert_eq!(STATE.load(&deps.storage).unwrap(), noncanonical);
}

#[test]
fn forward_excess_is_completed_alias_that_preserves_shares_and_updates_counters() {
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    instantiate_valid(deps.as_mut());
    save_releasing_state(&mut deps, Some(test_addr(BUYER)), 60);
    let mut completed = STATE.load(&deps.storage).unwrap();
    completed.status = DealStatus::Completed;
    completed.released_total_ngonka = Uint256::from(150_u128);
    completed.buyer_released_ngonka = Uint256::from(90_u128);
    completed.host_released_ngonka = Uint256::from(60_u128);
    STATE.save(deps.as_mut().storage, &completed).unwrap();
    deps.querier.base.bank.update_balance(
        mock_env().contract.address,
        vec![coin(10, NGONKA_DENOM), coin(91, "unrelated")],
    );

    let response = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr("permissionless-caller"), &[]),
        ExecuteMsg::ForwardExcessGnk {},
    )
    .unwrap();
    assert_eq!(response.messages.len(), 2);
    assert_bank_send(&response.messages[0], &test_addr(BUYER), Uint128::new(6));
    assert_bank_send(&response.messages[1], &test_addr(HOST), Uint128::new(4));
    assert_event_attributes(
        &response,
        "excess_gnk_forwarded",
        &[
            ("amount_ngonka", "10".to_string()),
            ("buyer_delta_ngonka", "6".to_string()),
            ("host_delta_ngonka", "4".to_string()),
            ("released_total_ngonka", "160".to_string()),
        ],
    );
    let after_forward = STATE.load(&deps.storage).unwrap();
    assert_eq!(after_forward.status, DealStatus::Completed);
    assert_eq!(after_forward.released_total_ngonka, Uint256::from(160_u128));
    assert_eq!(after_forward.buyer_released_ngonka, Uint256::from(96_u128));
    assert_eq!(after_forward.host_released_ngonka, Uint256::from(64_u128));

    deps.querier
        .base
        .bank
        .update_balance(mock_env().contract.address, vec![coin(91, "unrelated")]);
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("repeat-caller"), &[]),
            ExecuteMsg::ForwardExcessGnk {},
        )
        .unwrap_err(),
        ContractError::NothingToForward
    );
    assert_eq!(STATE.load(&deps.storage).unwrap(), after_forward);
}

#[test]
fn host_only_fallback_releases_zero_claim_refunded_and_expired_balances() {
    for status in [
        DealStatus::Completed,
        DealStatus::Refunded,
        DealStatus::Expired,
    ] {
        let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
        instantiate_valid(deps.as_mut());
        save_host_only_state(&mut deps, status.clone(), Some(test_addr(BUYER)));
        deps.querier
            .base
            .bank
            .update_balance(mock_env().contract.address, vec![coin(7, NGONKA_DENOM)]);

        let response = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("keeper"), &[]),
            ExecuteMsg::ReleaseUnlockedGnk {},
        )
        .unwrap();
        assert_eq!(response.messages.len(), 1);
        assert_bank_send(&response.messages[0], &test_addr(HOST), Uint128::new(7));
        let state = STATE.load(&deps.storage).unwrap();
        assert_eq!(state.status, status);
        assert_eq!(state.total_claim_ngonka, Uint128::zero());
        assert_eq!(state.buyer_released_ngonka, Uint256::zero());
        assert_eq!(state.host_released_ngonka, Uint256::from(7_u128));
        assert_eq!(state.released_total_ngonka, Uint256::from(7_u128));
    }
}

#[test]
fn release_status_clamps_original_remaining_after_lifetime_paid_exceeds_entitlement() {
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    instantiate_valid(deps.as_mut());
    save_releasing_state(&mut deps, Some(test_addr(BUYER)), 100);
    let mut state = STATE.load(&deps.storage).unwrap();
    state.status = DealStatus::Completed;
    let lifetime_paid = Uint256::from(Uint128::MAX) + Uint256::one();
    state.released_total_ngonka = lifetime_paid;
    state.buyer_released_ngonka = lifetime_paid;
    state.host_released_ngonka = Uint256::zero();
    STATE.save(deps.as_mut().storage, &state).unwrap();

    let release: ReleaseStatusResponse =
        from_json(query(deps.as_ref(), mock_env(), QueryMsg::ReleaseStatus {}).unwrap()).unwrap();
    assert_eq!(release.buyer_original_remaining_ngonka, Uint128::zero());
    assert_eq!(release.host_original_remaining_ngonka, Uint128::zero());
    assert_eq!(release.buyer_released_ngonka, lifetime_paid);
    assert_eq!(
        release.gnk_release_policy,
        GnkReleasePolicy::Proportional {
            buyer_share_numerator: Uint128::new(100),
            share_denominator: Uint128::new(100),
        }
    );
}

#[test]
fn release_and_forward_enforce_state_and_native_funds_before_queries() {
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    instantiate_valid(deps.as_mut());
    let original = STATE.load(&deps.storage).unwrap();

    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("caller"), &[]),
            ExecuteMsg::ReleaseUnlockedGnk {},
        )
        .unwrap_err(),
        ContractError::InvalidReleaseState {
            actual: DealStatus::Open,
        }
    );
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("caller"), &[]),
            ExecuteMsg::ForwardExcessGnk {},
        )
        .unwrap_err(),
        ContractError::ExcessForwardingBeforeCompletion {
            actual: DealStatus::Open,
        }
    );
    for msg in [
        ExecuteMsg::ReleaseUnlockedGnk {},
        ExecuteMsg::ForwardExcessGnk {},
    ] {
        assert!(matches!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("caller"), &[coin(1, NGONKA_DENOM)]),
                msg,
            ),
            Err(ContractError::Payment(_))
        ));
    }
    assert_eq!(STATE.load(&deps.storage).unwrap(), original);
}

#[test]
fn withdrawal_requires_a_nonzero_obligation_and_rejects_native_funds() {
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    prepare_locked(&mut deps, true);
    for role in [UsdtRole::Host, UsdtRole::Fee, UsdtRole::Buyer] {
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("keeper"), &[]),
                ExecuteMsg::WithdrawUsdt { role }
            )
            .unwrap_err(),
            ContractError::NothingToWithdraw
        );
    }
    set_performance(&mut deps, 11, test_addr(HOST).to_string(), 0, 0, true);
    execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr("keeper"), &[]),
        ExecuteMsg::SettleClaim {},
    )
    .unwrap();
    let pending = query_usdt_payments(deps.as_ref()).unwrap();
    assert!(execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr("keeper"), &[coin(1, "ngonka")]),
        ExecuteMsg::WithdrawUsdt {
            role: UsdtRole::Buyer
        }
    )
    .is_err());
    assert_eq!(query_usdt_payments(deps.as_ref()).unwrap(), pending);
    let response = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr("keeper"), &[]),
        ExecuteMsg::WithdrawUsdt {
            role: UsdtRole::Buyer,
        },
    )
    .unwrap();
    assert_cw20_transfer(
        &response.messages[0],
        &test_addr(TOKEN),
        &test_addr(BUYER),
        Uint128::new(100_000_000),
    );
    assert_eq!(
        query_usdt_payments(deps.as_ref())
            .unwrap()
            .buyer
            .paid_micro_usdt,
        Uint128::new(100_000_000)
    );
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("keeper"), &[]),
            ExecuteMsg::WithdrawUsdt {
                role: UsdtRole::Buyer
            }
        )
        .unwrap_err(),
        ContractError::NothingToWithdraw
    );
}

#[derive(Clone, Copy)]
enum SummaryFault {
    HandlerError,
    UnsupportedRequest,
    InvalidResponse,
    MalformedProtobuf,
    OversizedResponse,
    MissingNestedSummary,
    WrongHost,
    WrongEpoch,
    InvalidParticipantAddress,
}

fn inject_summary_fault(
    deps: &mut OwnedDeps<MockStorage, MockApi, DealMockQuerier, Empty>,
    fault: SummaryFault,
) -> (&'static str, GonkaQueryError) {
    use crate::gonka::{EPOCH_PERFORMANCE_PATH as ROUTE, MAX_GRPC_RESPONSE_BYTES as MAX};
    match fault {
        SummaryFault::HandlerError => {
            deps.querier.performance_result_override = Some(SystemResult::Ok(ContractResult::Err(
                "handler failure".into(),
            )));
            (
                "handler_error",
                GonkaQueryError::QueryFailed { route: ROUTE },
            )
        }
        SummaryFault::UnsupportedRequest => {
            deps.querier.performance_result_override =
                Some(SystemResult::Err(SystemError::UnsupportedRequest {
                    kind: "route".into(),
                }));
            (
                "unsupported_request",
                GonkaQueryError::UnsupportedRequest { route: ROUTE },
            )
        }
        SummaryFault::InvalidResponse => {
            deps.querier.performance_result_override =
                Some(SystemResult::Err(SystemError::InvalidResponse {
                    error: "invalid Go response envelope".into(),
                    response: Binary::from(b"not-json".as_slice()),
                }));
            (
                "invalid_response",
                GonkaQueryError::InvalidResponse { route: ROUTE },
            )
        }
        SummaryFault::MalformedProtobuf => {
            deps.querier.performance_response = Ok(Binary::from(vec![0xff]));
            (
                "malformed_protobuf",
                GonkaQueryError::DecodeFailed { route: ROUTE },
            )
        }
        SummaryFault::OversizedResponse => {
            deps.querier.performance_response = Ok(Binary::from(vec![0; MAX + 1]));
            (
                "oversized_response",
                GonkaQueryError::ResponseTooLarge {
                    route: ROUTE,
                    actual: MAX + 1,
                    maximum: MAX,
                },
            )
        }
        SummaryFault::MissingNestedSummary => {
            deps.querier.performance_response = Ok(Binary::default());
            (
                "missing_nested_summary",
                GonkaQueryError::MissingField {
                    route: ROUTE,
                    field: "epoch_performance_summary",
                },
            )
        }
        SummaryFault::WrongHost => {
            set_performance(deps, 11, test_addr("other-host").to_string(), 1, 2, true);
            (
                "wrong_host",
                GonkaQueryError::IdentityMismatch {
                    route: ROUTE,
                    field: "participant_id",
                },
            )
        }
        SummaryFault::WrongEpoch => {
            set_performance(deps, 12, test_addr(HOST).to_string(), 1, 2, true);
            (
                "wrong_epoch",
                GonkaQueryError::IdentityMismatch {
                    route: ROUTE,
                    field: "epoch_index",
                },
            )
        }
        SummaryFault::InvalidParticipantAddress => {
            set_performance(deps, 11, "x".into(), 1, 2, true);
            (
                "invalid_participant_address",
                GonkaQueryError::InvalidAddress {
                    route: ROUTE,
                    field: "participant_id",
                },
            )
        }
    }
}

// Faults are injected at the raw-query boundary; the production decoder and
// contract state machine are exercised over CosmWasm test dependencies.
// Package C freezes the short c_* entry-point names and C_CASE markers in
// ops/a8/verifier.py, so these wrappers retain that naming exception.
fn c_summary_policy(fault: SummaryFault) {
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    prepare_locked(&mut deps, true);
    set_current_epoch(&mut deps, 13); // E+2
    let (kind, expected) = inject_summary_fault(&mut deps, fault);
    let before = storage_snapshot(&deps.storage);
    for (suffix, msg) in [
        ("probe", ExecuteMsg::SettleClaim {}),
        ("e2", ExecuteMsg::Refund {}),
    ] {
        let error = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("caller"), &[]),
            msg,
        )
        .unwrap_err();
        match error {
            ContractError::Gonka(actual) => assert_eq!(actual, expected),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(storage_snapshot(&deps.storage), before);
        println!("C_CASE:r4-{kind}-{suffix}");
    }
    set_current_epoch(&mut deps, 14); // E+3, never E+2
    let state_before = STATE.load(&deps.storage).unwrap();
    let response = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr("caller"), &[]),
        ExecuteMsg::Refund {},
    )
    .unwrap();
    assert_network_unconfirmed_refund(&deps.storage, &response, state_before);
    let terminal = storage_snapshot(&deps.storage);
    println!("C_CASE:r4-{kind}-e3");
    deps.querier.performance_result_override = None;
    set_performance(&mut deps, 11, test_addr(HOST).to_string(), 1, 2, true);
    for (suffix, msg, error) in [
        (
            "terminal-refund",
            ExecuteMsg::Refund {},
            ContractError::InvalidRefundState {
                actual: DealStatus::Refunded,
            },
        ),
        (
            "terminal-settle_claim",
            ExecuteMsg::SettleClaim {},
            ContractError::InvalidSettlementState {
                actual: DealStatus::Refunded,
            },
        ),
    ] {
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("caller"), &[]),
                msg
            )
            .unwrap_err(),
            error
        );
        assert_eq!(storage_snapshot(&deps.storage), terminal);
        println!("C_CASE:r4-{kind}-{suffix}");
    }
}

#[test]
fn c_policy_handler_error() {
    c_summary_policy(SummaryFault::HandlerError);
}

#[test]
fn c_policy_malformed_protobuf() {
    c_summary_policy(SummaryFault::MalformedProtobuf);
}

#[test]
fn c_policy_oversized_response() {
    c_summary_policy(SummaryFault::OversizedResponse);
}

#[test]
fn c_policy_missing_nested_summary() {
    c_summary_policy(SummaryFault::MissingNestedSummary);
}

#[test]
fn c_policy_wrong_host() {
    c_summary_policy(SummaryFault::WrongHost);
}

#[test]
fn c_policy_wrong_epoch() {
    c_summary_policy(SummaryFault::WrongEpoch);
}

#[test]
fn c_policy_invalid_participant_address() {
    c_summary_policy(SummaryFault::InvalidParticipantAddress);
}

#[test]
fn c_policy_unsupported_request() {
    c_summary_policy(SummaryFault::UnsupportedRequest);
}

#[derive(Clone, Copy, PartialEq)]
enum RoutingFault {
    Epoch,
    HandlerError,
    MalformedProtobuf,
    DuplicateRouting,
}

fn c_routing_policy(fault: RoutingFault, refund: bool) {
    let mut deps = mock_deps(10, SETTLEMENT_TOKEN_DECIMALS);
    let op = if refund { "refund" } else { "lock" };
    if fault == RoutingFault::Epoch && refund {
        prepare_locked(&mut deps, true);
        set_current_epoch(&mut deps, 14);
        deps.querier.performance_result_override = Some(SystemResult::Ok(ContractResult::Err(
            "summary unavailable".into(),
        )));
    } else {
        prepare_funded_for_refund(
            &mut deps,
            11,
            vec![ClaimRecipientEntry {
                epoch: 11,
                recipient: mock_env().contract.address.to_string(),
            }],
        );
    }
    let before = storage_snapshot(&deps.storage);
    let state_before = STATE.load(&deps.storage).unwrap();
    let config_before = CONFIG.load(&deps.storage).unwrap();
    let route = crate::gonka::LIST_CLAIM_RECIPIENTS_PATH;
    let (kind, expected) = match fault {
        RoutingFault::Epoch => {
            deps.querier.current_epoch_override = Some(SystemResult::Ok(ContractResult::Err(
                "epoch unavailable".into(),
            )));
            (
                "epoch",
                ContractError::CommonGonka(
                    marketplace_common::error::GonkaQueryError::QueryFailed {
                        route: marketplace_common::gonka::GET_CURRENT_EPOCH_PATH,
                    },
                ),
            )
        }
        RoutingFault::HandlerError => {
            deps.querier.routing_override = Some(SystemResult::Ok(ContractResult::Err(
                "routing unavailable".into(),
            )));
            (
                "handler_error",
                ContractError::Gonka(GonkaQueryError::QueryFailed { route }),
            )
        }
        RoutingFault::MalformedProtobuf => {
            deps.querier.claim_recipients_response = Ok(Binary::from(vec![0xff]));
            (
                "malformed_protobuf",
                ContractError::Gonka(GonkaQueryError::DecodeFailed { route }),
            )
        }
        RoutingFault::DuplicateRouting => {
            set_claim_recipients(
                &mut deps,
                vec![
                    ClaimRecipientEntry {
                        epoch: 11,
                        recipient: mock_env().contract.address.to_string()
                    };
                    2
                ],
            );
            (
                "duplicate_routing",
                ContractError::Gonka(GonkaQueryError::DuplicateClaimRecipient { epoch: 11 }),
            )
        }
    };
    let msg = || {
        if refund {
            ExecuteMsg::Refund {}
        } else {
            ExecuteMsg::Lock {}
        }
    };
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("caller"), &[]),
            msg()
        )
        .unwrap_err(),
        expected
    );
    assert_eq!(storage_snapshot(&deps.storage), before);
    println!("C_CASE:r3-{kind}-{op}");
    // The frozen "healthy" marker means the tested epoch/routing query recovered.
    // Epoch-refund deliberately retains the summary fault, so its recovered
    // current-epoch query permits a NetworkUnconfirmed emergency refund.
    deps.querier.current_epoch_override = None;
    deps.querier.routing_override = None;
    set_claim_recipients(
        &mut deps,
        vec![ClaimRecipientEntry {
            epoch: 11,
            recipient: mock_env().contract.address.to_string(),
        }],
    );
    let result = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr("caller"), &[]),
        msg(),
    );
    if refund && fault != RoutingFault::Epoch {
        assert_eq!(
            result.unwrap_err(),
            ContractError::RefundRecipientStillRouted { epoch: 11 }
        );
        assert_eq!(storage_snapshot(&deps.storage), before);
    } else {
        let response = result.unwrap();
        assert_eq!(CONFIG.load(&deps.storage).unwrap(), config_before);
        if refund {
            assert_network_unconfirmed_refund(&deps.storage, &response, state_before);
        } else {
            assert_locked(&deps.storage, &response, state_before);
        }
    }
    println!("C_CASE:r3-{kind}-{op}-healthy");
}

#[test]
fn c_routing_handler_error_lock() {
    c_routing_policy(RoutingFault::HandlerError, false);
}

#[test]
fn c_routing_handler_error_refund() {
    c_routing_policy(RoutingFault::HandlerError, true);
}

#[test]
fn c_routing_malformed_protobuf_lock() {
    c_routing_policy(RoutingFault::MalformedProtobuf, false);
}

#[test]
fn c_routing_malformed_protobuf_refund() {
    c_routing_policy(RoutingFault::MalformedProtobuf, true);
}

#[test]
fn c_routing_duplicate_routing_lock() {
    c_routing_policy(RoutingFault::DuplicateRouting, false);
}

#[test]
fn c_routing_duplicate_routing_refund() {
    c_routing_policy(RoutingFault::DuplicateRouting, true);
}

#[test]
fn c_routing_epoch_lock() {
    c_routing_policy(RoutingFault::Epoch, false);
}

#[test]
fn c_routing_epoch_refund() {
    c_routing_policy(RoutingFault::Epoch, true);
}
