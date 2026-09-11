use std::{marker::PhantomData, sync::Arc};

use cosmwasm_std::{
    coin, from_json,
    testing::{message_info, mock_dependencies, mock_env, MockApi, MockQuerier, MockStorage},
    to_json_binary, Addr, Binary, ContractResult, CosmosMsg, Empty, GrpcQuery, MsgResponse,
    OwnedDeps, Querier, QuerierResult, QueryRequest, ReplyOn, SubMsgResponse, SubMsgResult,
    SystemError, SystemResult, Uint128, WasmMsg, WasmQuery,
};
use cw2::get_contract_version;
use cw20::TokenInfoResponse;
use gonka_proto::{QueryGetCurrentEpochRequest, QueryGetCurrentEpochResponse};
use marketplace_api::deal::InstantiateMsg as DealInstantiateMsg;
use prost::Message;

use super::*;

const CREATOR: &str = "creator000";
const TOKEN: &str = "token000";
const FEE_RECIPIENT: &str = "fees000";

fn test_addr(label: &str) -> Addr {
    cosmwasm_std::testing::MockApi::default().addr_make(label)
}

fn valid_instantiate_msg() -> InstantiateMsg {
    InstantiateMsg {
        deal_code_id: 7,
        settlement_cw20: test_addr(TOKEN).to_string(),
        fee_recipient: test_addr(FEE_RECIPIENT).to_string(),
        fee_bps: PROTOCOL_FEE_BPS,
    }
}

fn mock_token_info(deps: &mut cosmwasm_std::testing::MockQuerier, decimals: u8) {
    let token = test_addr(TOKEN).to_string();
    deps.update_wasm(move |query| match query {
        WasmQuery::Smart { contract_addr, .. } if contract_addr == &token => {
            SystemResult::Ok(ContractResult::Ok(
                to_json_binary(&TokenInfoResponse {
                    name: "Test USDT".to_string(),
                    symbol: "USDT".to_string(),
                    decimals,
                    total_supply: Uint128::new(1_000_000),
                })
                .unwrap(),
            ))
        }
        _ => SystemResult::Err(SystemError::UnsupportedRequest {
            kind: "unexpected wasm query".to_string(),
        }),
    });
}

struct GrpcMockQuerier {
    base: MockQuerier<Empty>,
    grpc_handler: Arc<dyn Fn(&GrpcQuery) -> QuerierResult + Send + Sync>,
}

impl Querier for GrpcMockQuerier {
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
            QueryRequest::Grpc(grpc) => (self.grpc_handler)(&grpc),
            other => self.base.handle_query(&other),
        }
    }
}

fn grpc_dependencies<F>(handler: F) -> OwnedDeps<MockStorage, MockApi, GrpcMockQuerier, Empty>
where
    F: Fn(&GrpcQuery) -> QuerierResult + Send + Sync + 'static,
{
    OwnedDeps {
        storage: MockStorage::default(),
        api: MockApi::default(),
        querier: GrpcMockQuerier {
            base: MockQuerier::new(&[]),
            grpc_handler: Arc::new(handler),
        },
        custom_query_type: PhantomData,
    }
}

fn initialized_create_offer_dependencies(
    epoch: u64,
) -> OwnedDeps<MockStorage, MockApi, GrpcMockQuerier, Empty> {
    let mut deps = grpc_dependencies(move |request| {
        assert_eq!(
            request.path,
            marketplace_common::gonka::GET_CURRENT_EPOCH_PATH
        );
        assert_eq!(
            QueryGetCurrentEpochRequest::decode(request.data.as_slice()).unwrap(),
            QueryGetCurrentEpochRequest {}
        );
        SystemResult::Ok(ContractResult::Ok(Binary::from(
            QueryGetCurrentEpochResponse { epoch }.encode_to_vec(),
        )))
    });
    CONFIG
        .save(
            deps.as_mut().storage,
            &FactoryConfig {
                deal_code_id: 7,
                settlement_cw20: test_addr(TOKEN),
                fee_recipient: test_addr(FEE_RECIPIENT),
                fee_bps: PROTOCOL_FEE_BPS,
            },
        )
        .unwrap();
    NEXT_DEAL_ID
        .save(deps.as_mut().storage, &FIRST_DEAL_ID)
        .unwrap();
    deps
}

fn pending_offer() -> PendingOffer {
    PendingOffer {
        deal_id: FIRST_DEAL_ID,
        host: test_addr("host000"),
        target_epoch: 11,
        price_micro_usdt_per_gnk: Uint128::new(1_000_000),
        buyer_budget_micro_usdt: Uint128::new(100_000_000),
    }
}

fn instantiate_response_data(address: &str) -> Binary {
    assert!(address.len() < 128);
    let mut data = vec![0x0a, address.len() as u8];
    data.extend_from_slice(address.as_bytes());
    Binary::from(data)
}

fn successful_reply(address: &str) -> Reply {
    #[allow(deprecated)]
    Reply {
        id: INSTANTIATE_DEAL_REPLY_ID,
        payload: Binary::default(),
        gas_used: 0,
        result: SubMsgResult::Ok(SubMsgResponse {
            events: vec![],
            data: None,
            msg_responses: vec![MsgResponse {
                type_url: crate::reply::INSTANTIATE_RESPONSE_TYPE_URL.to_string(),
                value: instantiate_response_data(address),
            }],
        }),
    }
}

fn assert_registry_empty(storage: &dyn cosmwasm_std::Storage) {
    assert!(DEALS.is_empty(storage));
    assert!(DEAL_BY_HOST_EPOCH.is_empty(storage));
}

#[test]
fn instantiate_validates_and_stores_immutable_config() {
    let mut deps = mock_dependencies();
    mock_token_info(&mut deps.querier, SETTLEMENT_TOKEN_DECIMALS);

    let response = instantiate(
        deps.as_mut(),
        mock_env(),
        message_info(&Addr::unchecked(CREATOR), &[]),
        valid_instantiate_msg(),
    )
    .unwrap();

    assert!(response.messages.is_empty());
    assert_eq!(NEXT_DEAL_ID.load(&deps.storage).unwrap(), FIRST_DEAL_ID);
    let config = CONFIG.load(&deps.storage).unwrap();
    assert_eq!(config.deal_code_id, 7);
    assert_eq!(config.settlement_cw20, test_addr(TOKEN));
    assert_eq!(config.fee_recipient, test_addr(FEE_RECIPIENT));
    assert_eq!(config.fee_bps, PROTOCOL_FEE_BPS);
    let version = get_contract_version(&deps.storage).unwrap();
    assert_eq!(version.contract, CONTRACT_NAME);
    assert_eq!(version.version, CONTRACT_VERSION);

    let response: ConfigResponse =
        from_json(query(deps.as_ref(), mock_env(), QueryMsg::Config {}).unwrap()).unwrap();
    assert_eq!(response.settlement_cw20, test_addr(TOKEN).to_string());
}

#[test]
fn instantiate_rejects_funds_code_id_fee_and_token_decimals() {
    let mut deps = mock_dependencies();
    mock_token_info(&mut deps.querier, SETTLEMENT_TOKEN_DECIMALS);
    let info = message_info(&Addr::unchecked(CREATOR), &[coin(1, "ngonka")]);
    assert!(matches!(
        instantiate(deps.as_mut(), mock_env(), info, valid_instantiate_msg()),
        Err(ContractError::Payment(_))
    ));

    let mut deps = mock_dependencies();
    mock_token_info(&mut deps.querier, SETTLEMENT_TOKEN_DECIMALS);
    let mut msg = valid_instantiate_msg();
    msg.deal_code_id = 0;
    assert_eq!(
        instantiate(
            deps.as_mut(),
            mock_env(),
            message_info(&Addr::unchecked(CREATOR), &[]),
            msg,
        )
        .unwrap_err(),
        ContractError::InvalidDealCodeId
    );

    let mut deps = mock_dependencies();
    mock_token_info(&mut deps.querier, SETTLEMENT_TOKEN_DECIMALS);
    let mut msg = valid_instantiate_msg();
    msg.fee_bps = 149;
    assert_eq!(
        instantiate(
            deps.as_mut(),
            mock_env(),
            message_info(&Addr::unchecked(CREATOR), &[]),
            msg,
        )
        .unwrap_err(),
        ContractError::InvalidFeeBps {
            expected: PROTOCOL_FEE_BPS,
            actual: 149,
        }
    );

    let mut deps = mock_dependencies();
    mock_token_info(&mut deps.querier, 18);
    assert_eq!(
        instantiate(
            deps.as_mut(),
            mock_env(),
            message_info(&Addr::unchecked(CREATOR), &[]),
            valid_instantiate_msg(),
        )
        .unwrap_err(),
        ContractError::InvalidSettlementDecimals {
            expected: SETTLEMENT_TOKEN_DECIMALS,
            actual: 18,
        }
    );

    let mut deps = mock_dependencies();
    assert_eq!(
        instantiate(
            deps.as_mut(),
            mock_env(),
            message_info(&Addr::unchecked(CREATOR), &[]),
            valid_instantiate_msg(),
        )
        .unwrap_err(),
        ContractError::InvalidSettlementToken
    );

    let mut deps = mock_dependencies();
    let mut msg = valid_instantiate_msg();
    msg.settlement_cw20 = "x".to_string();
    assert!(matches!(
        instantiate(
            deps.as_mut(),
            mock_env(),
            message_info(&Addr::unchecked(CREATOR), &[]),
            msg,
        ),
        Err(ContractError::Std(_))
    ));
}

#[test]
fn create_offer_uses_caller_and_factory_config_in_exact_submessage() {
    let mut deps = initialized_create_offer_dependencies(10);
    let env = mock_env();
    let host = test_addr("host000");

    let response = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&host, &[]),
        ExecuteMsg::CreateOffer {
            target_epoch: 11,
            price_micro_usdt_per_gnk: Uint128::new(1_000_000),
            buyer_budget_micro_usdt: Uint128::new(100_000_000),
        },
    )
    .unwrap();

    assert_eq!(response.messages.len(), 1);
    let submessage = &response.messages[0];
    assert_eq!(submessage.id, INSTANTIATE_DEAL_REPLY_ID);
    assert_eq!(submessage.reply_on, ReplyOn::Success);
    let CosmosMsg::Wasm(WasmMsg::Instantiate {
        admin,
        code_id,
        msg,
        funds,
        label,
    }) = &submessage.msg
    else {
        panic!("expected Deal instantiate submessage")
    };
    assert_eq!(admin, &None);
    assert_eq!(*code_id, 7);
    assert!(funds.is_empty());
    assert_eq!(label, "gonka-forward-deal-1");

    let deal_msg: DealInstantiateMsg = from_json(msg).unwrap();
    assert_eq!(deal_msg.factory, env.contract.address.to_string());
    assert_eq!(deal_msg.host, host.to_string());
    assert_eq!(deal_msg.target_epoch, 11);
    assert_eq!(deal_msg.price_micro_usdt_per_gnk, Uint128::new(1_000_000));
    assert_eq!(deal_msg.buyer_budget_micro_usdt, Uint128::new(100_000_000));
    assert_eq!(deal_msg.settlement_cw20, test_addr(TOKEN).to_string());
    assert_eq!(deal_msg.fee_recipient, test_addr(FEE_RECIPIENT).to_string());
    assert_eq!(deal_msg.fee_bps, PROTOCOL_FEE_BPS);
    assert_eq!(deal_msg.pinned_gonka_sha, gonka_proto::SOURCE_COMMIT);
    assert_eq!(PENDING_OFFER.load(&deps.storage).unwrap(), pending_offer());
    assert_registry_empty(&deps.storage);
    assert_eq!(NEXT_DEAL_ID.load(&deps.storage).unwrap(), FIRST_DEAL_ID);
}

#[test]
fn create_offer_rejects_funds_invalid_terms_epoch_and_native_query_failure() {
    let mut deps = mock_dependencies();
    assert!(matches!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("host000"), &[coin(1, "ngonka")]),
            ExecuteMsg::CreateOffer {
                target_epoch: 11,
                price_micro_usdt_per_gnk: Uint128::one(),
                buyer_budget_micro_usdt: Uint128::one(),
            },
        ),
        Err(ContractError::Payment(_))
    ));

    for (price, budget, expected) in [
        (Uint128::zero(), Uint128::one(), ContractError::ZeroPrice),
        (Uint128::one(), Uint128::zero(), ContractError::ZeroBudget),
    ] {
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("host000"), &[]),
                ExecuteMsg::CreateOffer {
                    target_epoch: 11,
                    price_micro_usdt_per_gnk: price,
                    buyer_budget_micro_usdt: budget,
                },
            )
            .unwrap_err(),
            expected
        );
    }

    let mut deps = initialized_create_offer_dependencies(10);
    for (target_epoch, expected) in [
        (
            10,
            ContractError::InvalidTargetEpoch {
                current: 10,
                target: 10,
            },
        ),
        (
            51,
            ContractError::EpochLookaheadExceeded {
                target: 51,
                maximum: 50,
            },
        ),
    ] {
        assert_eq!(
            execute(
                deps.as_mut(),
                mock_env(),
                message_info(&test_addr("host000"), &[]),
                ExecuteMsg::CreateOffer {
                    target_epoch,
                    price_micro_usdt_per_gnk: Uint128::one(),
                    buyer_budget_micro_usdt: Uint128::one(),
                },
            )
            .unwrap_err(),
            expected
        );
    }
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("host000"), &[]),
            ExecuteMsg::CreateOffer {
                target_epoch: 11,
                price_micro_usdt_per_gnk: Uint128::new(1_000_000_001),
                buyer_budget_micro_usdt: Uint128::one(),
            },
        )
        .unwrap_err(),
        ContractError::ZeroCapacity
    );
    assert!(matches!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("host000"), &[]),
            ExecuteMsg::CreateOffer {
                target_epoch: 11,
                price_micro_usdt_per_gnk: Uint128::one(),
                buyer_budget_micro_usdt: Uint128::MAX,
            },
        ),
        Err(ContractError::Math(
            marketplace_common::error::MathError::ArithmeticOverflow { .. }
        ))
    ));

    let mut deps = initialized_create_offer_dependencies(u64::MAX - 1);
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("host000"), &[]),
            ExecuteMsg::CreateOffer {
                target_epoch: u64::MAX,
                price_micro_usdt_per_gnk: Uint128::one(),
                buyer_budget_micro_usdt: Uint128::one(),
            },
        )
        .unwrap_err(),
        ContractError::EpochLookaheadOverflow
    );

    let mut deps = grpc_dependencies(|_| {
        SystemResult::Err(SystemError::UnsupportedRequest {
            kind: "private node error".to_string(),
        })
    });
    let error = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&test_addr("host000"), &[]),
        ExecuteMsg::CreateOffer {
            target_epoch: 11,
            price_micro_usdt_per_gnk: Uint128::one(),
            buyer_budget_micro_usdt: Uint128::one(),
        },
    )
    .unwrap_err();
    assert_eq!(
        error,
        ContractError::Gonka(marketplace_common::error::GonkaQueryError::QueryFailed {
            route: marketplace_common::gonka::GET_CURRENT_EPOCH_PATH,
        })
    );
    assert!(!error.to_string().contains("private node error"));

    let mut deps =
        grpc_dependencies(|_| SystemResult::Ok(ContractResult::Ok(Binary::from(vec![0xff]))));
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("host000"), &[]),
            ExecuteMsg::CreateOffer {
                target_epoch: 11,
                price_micro_usdt_per_gnk: Uint128::one(),
                buyer_budget_micro_usdt: Uint128::one(),
            },
        )
        .unwrap_err(),
        ContractError::Gonka(marketplace_common::error::GonkaQueryError::DecodeFailed {
            route: marketplace_common::gonka::GET_CURRENT_EPOCH_PATH,
        })
    );

    let mut deps = grpc_dependencies(|_| {
        SystemResult::Ok(ContractResult::Ok(Binary::from(vec![0; 32 * 1024 + 1])))
    });
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("host000"), &[]),
            ExecuteMsg::CreateOffer {
                target_epoch: 11,
                price_micro_usdt_per_gnk: Uint128::one(),
                buyer_budget_micro_usdt: Uint128::one(),
            },
        )
        .unwrap_err(),
        ContractError::Gonka(
            marketplace_common::error::GonkaQueryError::ResponseTooLarge {
                route: marketplace_common::gonka::GET_CURRENT_EPOCH_PATH,
                actual: 32 * 1024 + 1,
                maximum: 32 * 1024,
            }
        )
    );
}

#[test]
fn create_offer_protects_uniqueness_and_pending_context() {
    let host = test_addr("host000");
    let mut deps = initialized_create_offer_dependencies(10);
    DEAL_BY_HOST_EPOCH
        .save(
            deps.as_mut().storage,
            (host.clone(), 11),
            &test_addr("existing-deal"),
        )
        .unwrap();
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&host, &[]),
            ExecuteMsg::CreateOffer {
                target_epoch: 11,
                price_micro_usdt_per_gnk: Uint128::one(),
                buyer_budget_micro_usdt: Uint128::one(),
            },
        )
        .unwrap_err(),
        ContractError::DuplicateOffer {
            host: host.to_string(),
            epoch: 11,
        }
    );

    DEAL_BY_HOST_EPOCH.remove(deps.as_mut().storage, (host.clone(), 11));
    let original = pending_offer();
    PENDING_OFFER
        .save(deps.as_mut().storage, &original)
        .unwrap();
    assert_eq!(
        execute(
            deps.as_mut(),
            mock_env(),
            message_info(&test_addr("another-host"), &[]),
            ExecuteMsg::CreateOffer {
                target_epoch: 12,
                price_micro_usdt_per_gnk: Uint128::one(),
                buyer_budget_micro_usdt: Uint128::one(),
            },
        )
        .unwrap_err(),
        ContractError::PendingOfferExists
    );
    assert_eq!(PENDING_OFFER.load(&deps.storage).unwrap(), original);
}

#[test]
fn successful_reply_indexes_once_advances_id_and_emits_offer_created() {
    let mut deps = mock_dependencies();
    let pending = pending_offer();
    PENDING_OFFER.save(deps.as_mut().storage, &pending).unwrap();
    NEXT_DEAL_ID
        .save(deps.as_mut().storage, &pending.deal_id)
        .unwrap();
    let deal = test_addr("deal000");

    let response = reply(deps.as_mut(), mock_env(), successful_reply(deal.as_str())).unwrap();
    assert_eq!(DEALS.load(&deps.storage, pending.deal_id).unwrap(), deal);
    assert_eq!(
        DEAL_BY_HOST_EPOCH
            .load(&deps.storage, (pending.host.clone(), pending.target_epoch))
            .unwrap(),
        deal
    );
    assert_eq!(NEXT_DEAL_ID.load(&deps.storage).unwrap(), 2);
    assert!(!PENDING_OFFER.exists(&deps.storage));
    assert_eq!(response.events.len(), 1);
    let event = &response.events[0];
    assert_eq!(event.ty, "offer_created");
    for (key, value) in [
        ("deal_id", "1".to_string()),
        ("deal", deal.to_string()),
        ("host", pending.host.to_string()),
        ("target_epoch", "11".to_string()),
        ("price_micro_usdt_per_gnk", "1000000".to_string()),
        ("buyer_budget_micro_usdt", "100000000".to_string()),
    ] {
        assert!(event
            .attributes
            .iter()
            .any(|attribute| attribute.key == key && attribute.value == value));
    }

    assert_eq!(
        reply(deps.as_mut(), mock_env(), successful_reply(deal.as_str())).unwrap_err(),
        ContractError::MissingPendingOffer
    );
    assert_eq!(NEXT_DEAL_ID.load(&deps.storage).unwrap(), 2);
}

#[test]
#[allow(deprecated)]
fn unexpected_or_malformed_replies_do_not_mutate_registry() {
    let variants = vec![
        (
            Reply {
                id: 77,
                ..successful_reply(test_addr("deal000").as_str())
            },
            ContractError::UnexpectedReplyId { id: 77 },
        ),
        (
            Reply {
                result: SubMsgResult::Err("instantiate failed".to_string()),
                ..successful_reply(test_addr("deal000").as_str())
            },
            ContractError::DealInstantiationFailed,
        ),
        (
            Reply {
                result: SubMsgResult::Ok(SubMsgResponse {
                    events: vec![],
                    data: None,
                    msg_responses: vec![],
                }),
                ..successful_reply(test_addr("deal000").as_str())
            },
            ContractError::UnexpectedInstantiateResponseCount { actual: 0 },
        ),
        (
            Reply {
                result: SubMsgResult::Ok(SubMsgResponse {
                    events: vec![],
                    data: None,
                    msg_responses: vec![
                        MsgResponse {
                            type_url: crate::reply::INSTANTIATE_RESPONSE_TYPE_URL.to_string(),
                            value: instantiate_response_data(test_addr("deal000").as_str()),
                        },
                        MsgResponse {
                            type_url: crate::reply::INSTANTIATE_RESPONSE_TYPE_URL.to_string(),
                            value: instantiate_response_data(test_addr("deal001").as_str()),
                        },
                    ],
                }),
                ..successful_reply(test_addr("deal000").as_str())
            },
            ContractError::UnexpectedInstantiateResponseCount { actual: 2 },
        ),
        (
            Reply {
                result: SubMsgResult::Ok(SubMsgResponse {
                    events: vec![],
                    data: None,
                    msg_responses: vec![MsgResponse {
                        type_url: "/wrong.type".to_string(),
                        value: Binary::default(),
                    }],
                }),
                ..successful_reply(test_addr("deal000").as_str())
            },
            ContractError::UnexpectedInstantiateResponseType {
                actual: "/wrong.type".to_string(),
            },
        ),
        (
            Reply {
                result: SubMsgResult::Ok(SubMsgResponse {
                    events: vec![],
                    data: None,
                    msg_responses: vec![MsgResponse {
                        type_url: crate::reply::INSTANTIATE_RESPONSE_TYPE_URL.to_string(),
                        value: Binary::from(vec![0xff]),
                    }],
                }),
                ..successful_reply(test_addr("deal000").as_str())
            },
            ContractError::InvalidInstantiateResponse,
        ),
        (successful_reply("x"), ContractError::InvalidDealAddress),
    ];

    for (reply_msg, expected) in variants {
        let mut deps = mock_dependencies();
        let pending = pending_offer();
        PENDING_OFFER.save(deps.as_mut().storage, &pending).unwrap();
        NEXT_DEAL_ID
            .save(deps.as_mut().storage, &pending.deal_id)
            .unwrap();
        assert_eq!(
            reply(deps.as_mut(), mock_env(), reply_msg).unwrap_err(),
            expected
        );
        assert_registry_empty(&deps.storage);
        assert_eq!(NEXT_DEAL_ID.load(&deps.storage).unwrap(), pending.deal_id);
        assert_eq!(PENDING_OFFER.load(&deps.storage).unwrap(), pending);
    }

    let mut deps = mock_dependencies();
    assert_eq!(
        reply(
            deps.as_mut(),
            mock_env(),
            successful_reply(test_addr("deal000").as_str())
        )
        .unwrap_err(),
        ContractError::MissingPendingOffer
    );
    assert_registry_empty(&deps.storage);
}

#[test]
fn reply_rejects_pending_inconsistency_collisions_and_id_overflow_before_writes() {
    #[derive(Clone, Copy)]
    enum Setup {
        MismatchedId,
        IdCollision,
        HostEpochCollision,
        Overflow,
    }

    for setup in [
        Setup::MismatchedId,
        Setup::IdCollision,
        Setup::HostEpochCollision,
        Setup::Overflow,
    ] {
        let mut deps = mock_dependencies();
        let mut pending = pending_offer();
        let next = match setup {
            Setup::MismatchedId => 2,
            Setup::Overflow => {
                pending.deal_id = u64::MAX;
                u64::MAX
            }
            _ => pending.deal_id,
        };
        PENDING_OFFER.save(deps.as_mut().storage, &pending).unwrap();
        NEXT_DEAL_ID.save(deps.as_mut().storage, &next).unwrap();
        let expected = match setup {
            Setup::MismatchedId => ContractError::PendingDealIdMismatch {
                pending: pending.deal_id,
                next,
            },
            Setup::IdCollision => {
                DEALS
                    .save(
                        deps.as_mut().storage,
                        pending.deal_id,
                        &test_addr("existing-deal"),
                    )
                    .unwrap();
                ContractError::DealIdAlreadyRegistered {
                    id: pending.deal_id,
                }
            }
            Setup::HostEpochCollision => {
                DEAL_BY_HOST_EPOCH
                    .save(
                        deps.as_mut().storage,
                        (pending.host.clone(), pending.target_epoch),
                        &test_addr("existing-deal"),
                    )
                    .unwrap();
                ContractError::DuplicateOffer {
                    host: pending.host.to_string(),
                    epoch: pending.target_epoch,
                }
            }
            Setup::Overflow => ContractError::DealIdOverflow,
        };

        assert_eq!(
            reply(
                deps.as_mut(),
                mock_env(),
                successful_reply(test_addr("new-deal").as_str())
            )
            .unwrap_err(),
            expected
        );
        assert_eq!(NEXT_DEAL_ID.load(&deps.storage).unwrap(), next);
        assert_eq!(PENDING_OFFER.load(&deps.storage).unwrap(), pending);
        assert!(!DEALS.has(&deps.storage, pending.deal_id) || matches!(setup, Setup::IdCollision));
        assert!(
            !DEAL_BY_HOST_EPOCH.has(&deps.storage, (pending.host.clone(), pending.target_epoch))
                || matches!(setup, Setup::HostEpochCollision)
        );
    }
}

#[test]
fn deal_queries_use_exact_keys_and_bounded_exclusive_pagination() {
    let mut deps = mock_dependencies();
    let host = test_addr("host000");
    for id in 1..=60 {
        DEALS
            .save(
                deps.as_mut().storage,
                id,
                &Addr::unchecked(format!("deal{id:03}")),
            )
            .unwrap();
    }
    DEAL_BY_HOST_EPOCH
        .save(
            deps.as_mut().storage,
            (host.clone(), 42),
            &Addr::unchecked("deal042"),
        )
        .unwrap();

    let by_id: DealResponse =
        from_json(query(deps.as_ref(), mock_env(), QueryMsg::Deal { id: 42 }).unwrap()).unwrap();
    assert_eq!(by_id.address, "deal042");

    let by_host: DealResponse = from_json(
        query(
            deps.as_ref(),
            mock_env(),
            QueryMsg::DealByHostEpoch {
                host: host.to_string(),
                epoch: 42,
            },
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(by_host.address, "deal042");

    let listed: ListDealsResponse = from_json(
        query(
            deps.as_ref(),
            mock_env(),
            QueryMsg::ListDeals {
                start_after: Some(5),
                limit: Some(500),
            },
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(listed.deals.len(), MAX_LIST_LIMIT as usize);
    assert_eq!(listed.deals.first().unwrap().id, 6);
    assert_eq!(listed.deals.last().unwrap().id, 55);
}
