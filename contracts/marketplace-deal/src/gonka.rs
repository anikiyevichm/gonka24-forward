//! Typed Gonka queries and fail-closed semantic validation.
//!
//! This boundary is intentionally implemented before the Deal state machine.
#![allow(dead_code)]

use cosmwasm_std::{
    to_json_vec, Addr, Binary, ContractResult, Deps, Empty, GrpcQuery, QuerierWrapper,
    QueryRequest, SystemError, SystemResult, Uint128,
};
use gonka_proto::{
    QueryEpochPerformanceSummaryByParticipantRequest,
    QueryEpochPerformanceSummaryByParticipantResponse, QueryListClaimRecipientsRequest,
    QueryListClaimRecipientsResponse, QueryTotalVestingAmountRequest,
    QueryTotalVestingAmountResponse,
};
use prost::Message;

use crate::error::GonkaQueryError;

pub(crate) const LIST_CLAIM_RECIPIENTS_PATH: &str =
    "/inference.inference.Query/ListClaimRecipients";
pub(crate) const EPOCH_PERFORMANCE_PATH: &str =
    "/inference.inference.Query/EpochPerformanceSummaryByParticipant";
pub(crate) const TOTAL_VESTING_AMOUNT_PATH: &str =
    "/inference.streamvesting.Query/TotalVestingAmount";
const BANK_BALANCE_ROUTE: &str = "bank/balance";

pub(crate) const NGONKA_DENOM: &str = "ngonka";
pub(crate) const MAX_GRPC_RESPONSE_BYTES: usize = 32 * 1024;
pub(crate) const MAX_CLAIM_RECIPIENT_ENTRIES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParticipantPerformance {
    pub claimed: bool,
    pub work_amount: Uint128,
    pub reward_amount: Uint128,
    pub total_claim_amount: Uint128,
}

impl GonkaQueryError {
    /// Only these failures can mean that a correctly formed, immutable summary
    /// request could not be answered or validated by the network. Request,
    /// arithmetic, state and unrelated system failures remain fail-closed.
    pub(crate) fn permits_network_unconfirmed_refund(&self) -> bool {
        matches!(
            self,
            Self::QueryFailed { .. }
                | Self::UnsupportedRequest { .. }
                | Self::InvalidResponse { .. }
                | Self::ResponseTooLarge { .. }
                | Self::DecodeFailed { .. }
                | Self::MissingField { .. }
                | Self::IdentityMismatch { .. }
                | Self::InvalidAddress { .. }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClaimRecipientRouting {
    Missing,
    RoutedTo(Addr),
}

pub(crate) fn query_claim_recipient_routing(
    deps: Deps,
    participant: &Addr,
    target_epoch: u64,
) -> Result<ClaimRecipientRouting, GonkaQueryError> {
    let response: QueryListClaimRecipientsResponse = query_proto(
        &deps.querier,
        LIST_CLAIM_RECIPIENTS_PATH,
        &QueryListClaimRecipientsRequest {
            participant: participant.to_string(),
        },
    )?;

    if response.entries.len() > MAX_CLAIM_RECIPIENT_ENTRIES {
        return Err(GonkaQueryError::TooManyClaimRecipients {
            actual: response.entries.len(),
            maximum: MAX_CLAIM_RECIPIENT_ENTRIES,
        });
    }

    let mut matches = response
        .entries
        .into_iter()
        .filter(|entry| entry.epoch == target_epoch);
    let Some(entry) = matches.next() else {
        return Ok(ClaimRecipientRouting::Missing);
    };
    if matches.next().is_some() {
        return Err(GonkaQueryError::DuplicateClaimRecipient {
            epoch: target_epoch,
        });
    }

    let returned_recipient =
        deps.api
            .addr_validate(&entry.recipient)
            .map_err(|_| GonkaQueryError::InvalidAddress {
                route: LIST_CLAIM_RECIPIENTS_PATH,
                field: "recipient",
            })?;

    Ok(ClaimRecipientRouting::RoutedTo(returned_recipient))
}

pub(crate) fn query_claim_recipient(
    deps: Deps,
    participant: &Addr,
    target_epoch: u64,
    expected_deal: &Addr,
) -> Result<(), GonkaQueryError> {
    match query_claim_recipient_routing(deps, participant, target_epoch)? {
        ClaimRecipientRouting::Missing => Err(GonkaQueryError::ClaimRecipientNotFound {
            epoch: target_epoch,
        }),
        ClaimRecipientRouting::RoutedTo(returned_recipient)
            if returned_recipient != *expected_deal =>
        {
            Err(GonkaQueryError::IdentityMismatch {
                route: LIST_CLAIM_RECIPIENTS_PATH,
                field: "recipient",
            })
        }
        ClaimRecipientRouting::RoutedTo(_) => Ok(()),
    }
}

/// Returns only a positively present, identity-checked performance summary.
///
/// This adapter deliberately preserves the two-level raw query result. The
/// emergency refund policy can therefore distinguish a normal native query
/// failure or typed unavailable route/invalid response from failures that do
/// not establish the origin of the problem. No error text is parsed.
pub(crate) fn query_epoch_performance(
    deps: Deps,
    participant: &Addr,
    epoch: u64,
) -> Result<ParticipantPerformance, GonkaQueryError> {
    let response: QueryEpochPerformanceSummaryByParticipantResponse = query_summary_proto(
        &deps.querier,
        EPOCH_PERFORMANCE_PATH,
        &QueryEpochPerformanceSummaryByParticipantRequest {
            epoch_index: epoch,
            participant_id: participant.to_string(),
        },
    )?;
    let summary = response
        .epoch_performance_summary
        .ok_or(GonkaQueryError::MissingField {
            route: EPOCH_PERFORMANCE_PATH,
            field: "epoch_performance_summary",
        })?;

    if summary.epoch_index != epoch {
        return Err(GonkaQueryError::IdentityMismatch {
            route: EPOCH_PERFORMANCE_PATH,
            field: "epoch_index",
        });
    }
    let returned_participant = deps
        .api
        .addr_validate(&summary.participant_id)
        .map_err(|_| GonkaQueryError::InvalidAddress {
            route: EPOCH_PERFORMANCE_PATH,
            field: "participant_id",
        })?;
    if returned_participant != *participant {
        return Err(GonkaQueryError::IdentityMismatch {
            route: EPOCH_PERFORMANCE_PATH,
            field: "participant_id",
        });
    }

    let work_amount = Uint128::from(summary.earned_coins);
    let reward_amount = Uint128::from(summary.rewarded_coins);
    let total_claim_amount =
        checked_add_amounts(work_amount, reward_amount, "earned_coins + rewarded_coins")?;

    Ok(ParticipantPerformance {
        claimed: summary.claimed,
        work_amount,
        reward_amount,
        total_claim_amount,
    })
}

fn query_summary_proto<Request, Response>(
    querier: &QuerierWrapper,
    route: &'static str,
    request: &Request,
) -> Result<Response, GonkaQueryError>
where
    Request: Message,
    Response: Message + Default,
{
    let mut request_bytes = Vec::new();
    request
        .encode(&mut request_bytes)
        .map_err(|_| GonkaQueryError::EncodeFailed { route })?;

    let query: QueryRequest<Empty> = QueryRequest::Grpc(GrpcQuery {
        path: route.to_string(),
        data: Binary::from(request_bytes),
    });
    let encoded_query = to_json_vec(&query).map_err(|_| GonkaQueryError::EncodeFailed { route })?;

    let response_bytes = match querier.raw_query(&encoded_query) {
        SystemResult::Ok(ContractResult::Ok(response)) => response,
        SystemResult::Ok(ContractResult::Err(_)) => {
            return Err(GonkaQueryError::QueryFailed { route });
        }
        SystemResult::Err(SystemError::UnsupportedRequest { .. }) => {
            return Err(GonkaQueryError::UnsupportedRequest { route });
        }
        SystemResult::Err(SystemError::InvalidResponse { .. }) => {
            return Err(GonkaQueryError::InvalidResponse { route });
        }
        SystemResult::Err(_) => {
            return Err(GonkaQueryError::UnexpectedSystemError { route });
        }
    };

    validate_response_size(route, response_bytes.len())?;
    Response::decode(response_bytes.as_slice()).map_err(|_| GonkaQueryError::DecodeFailed { route })
}

pub(crate) fn query_total_vesting(
    querier: &QuerierWrapper,
    deal: &Addr,
) -> Result<Uint128, GonkaQueryError> {
    let response: QueryTotalVestingAmountResponse = query_proto(
        querier,
        TOTAL_VESTING_AMOUNT_PATH,
        &QueryTotalVestingAmountRequest {
            participant_address: deal.to_string(),
        },
    )?;

    let mut amount = None;
    for coin in response.total_amount {
        if coin.denom != NGONKA_DENOM {
            return Err(GonkaQueryError::UnexpectedDenom {
                route: TOTAL_VESTING_AMOUNT_PATH,
            });
        }
        if amount.is_some() {
            return Err(GonkaQueryError::DuplicateDenom {
                route: TOTAL_VESTING_AMOUNT_PATH,
            });
        }
        amount =
            Some(
                coin.amount
                    .parse::<Uint128>()
                    .map_err(|_| GonkaQueryError::InvalidAmount {
                        route: TOTAL_VESTING_AMOUNT_PATH,
                    })?,
            );
    }

    Ok(amount.unwrap_or_default())
}

pub(crate) fn query_ngonka_balance(
    querier: &QuerierWrapper,
    deal: &Addr,
) -> Result<Uint128, GonkaQueryError> {
    let coin =
        querier
            .query_balance(deal, NGONKA_DENOM)
            .map_err(|_| GonkaQueryError::QueryFailed {
                route: BANK_BALANCE_ROUTE,
            })?;
    if coin.denom != NGONKA_DENOM {
        return Err(GonkaQueryError::UnexpectedDenom {
            route: BANK_BALANCE_ROUTE,
        });
    }
    Ok(coin.amount)
}

fn query_proto<Request, Response>(
    querier: &QuerierWrapper,
    route: &'static str,
    request: &Request,
) -> Result<Response, GonkaQueryError>
where
    Request: Message,
    Response: Message + Default,
{
    let mut request_bytes = Vec::new();
    request
        .encode(&mut request_bytes)
        .map_err(|_| GonkaQueryError::EncodeFailed { route })?;

    let response_bytes = querier
        .query_grpc(route.to_string(), Binary::from(request_bytes))
        .map_err(|_| GonkaQueryError::QueryFailed { route })?;
    validate_response_size(route, response_bytes.len())?;

    Response::decode(response_bytes.as_slice()).map_err(|_| GonkaQueryError::DecodeFailed { route })
}

fn validate_response_size(route: &'static str, actual: usize) -> Result<(), GonkaQueryError> {
    if actual > MAX_GRPC_RESPONSE_BYTES {
        return Err(GonkaQueryError::ResponseTooLarge {
            route,
            actual,
            maximum: MAX_GRPC_RESPONSE_BYTES,
        });
    }

    Ok(())
}

fn checked_add_amounts(
    left: Uint128,
    right: Uint128,
    operation: &'static str,
) -> Result<Uint128, GonkaQueryError> {
    left.checked_add(right)
        .map_err(|_| GonkaQueryError::ArithmeticOverflow { operation })
}

#[cfg(test)]
mod tests {
    use std::{marker::PhantomData, sync::Arc};

    use cosmwasm_std::{
        coin, from_json,
        testing::{MockApi, MockQuerier, MockStorage},
        ContractResult, Empty, GrpcQuery, OwnedDeps, Querier, QuerierResult, QueryRequest,
        SystemError, SystemResult,
    };
    use gonka_proto::{ClaimRecipientEntry, Coin, EpochPerformanceSummary};

    use super::*;

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

    fn protobuf_ok(message: &impl Message) -> QuerierResult {
        SystemResult::Ok(ContractResult::Ok(Binary::from(message.encode_to_vec())))
    }

    #[test]
    fn response_size_limit_accepts_exact_boundary() {
        assert_eq!(
            validate_response_size(EPOCH_PERFORMANCE_PATH, MAX_GRPC_RESPONSE_BYTES),
            Ok(())
        );
        assert_eq!(
            validate_response_size(EPOCH_PERFORMANCE_PATH, MAX_GRPC_RESPONSE_BYTES + 1),
            Err(GonkaQueryError::ResponseTooLarge {
                route: EPOCH_PERFORMANCE_PATH,
                actual: MAX_GRPC_RESPONSE_BYTES + 1,
                maximum: MAX_GRPC_RESPONSE_BYTES,
            })
        );
    }

    #[test]
    fn claim_recipient_checks_request_and_exact_deal_identity() {
        let api = MockApi::default();
        let host = api.addr_make("host");
        let deal = api.addr_make("deal");
        let expected_host = host.to_string();
        let expected_deal = deal.to_string();
        let deps = grpc_dependencies(move |request| {
            assert_eq!(request.path, LIST_CLAIM_RECIPIENTS_PATH);
            let decoded = QueryListClaimRecipientsRequest::decode(request.data.as_slice()).unwrap();
            assert_eq!(decoded.participant, expected_host);
            protobuf_ok(&QueryListClaimRecipientsResponse {
                entries: vec![
                    ClaimRecipientEntry {
                        epoch: 6,
                        recipient: "ignored-for-another-epoch".to_string(),
                    },
                    ClaimRecipientEntry {
                        epoch: 7,
                        recipient: expected_deal.clone(),
                    },
                ],
            })
        });

        query_claim_recipient(deps.as_ref(), &host, 7, &deal).unwrap();
    }

    #[test]
    fn missing_duplicate_and_excessive_claim_recipient_entries_fail_closed() {
        let api = MockApi::default();
        let host = api.addr_make("host");
        let deal = api.addr_make("deal");

        let missing = grpc_dependencies(|_| {
            protobuf_ok(&QueryListClaimRecipientsResponse { entries: vec![] })
        });
        assert_eq!(
            query_claim_recipient(missing.as_ref(), &host, 7, &deal).unwrap_err(),
            GonkaQueryError::ClaimRecipientNotFound { epoch: 7 }
        );

        let deal_string = deal.to_string();
        let duplicate = grpc_dependencies(move |_| {
            protobuf_ok(&QueryListClaimRecipientsResponse {
                entries: vec![
                    ClaimRecipientEntry {
                        epoch: 7,
                        recipient: deal_string.clone(),
                    },
                    ClaimRecipientEntry {
                        epoch: 7,
                        recipient: deal_string.clone(),
                    },
                ],
            })
        });
        assert_eq!(
            query_claim_recipient(duplicate.as_ref(), &host, 7, &deal).unwrap_err(),
            GonkaQueryError::DuplicateClaimRecipient { epoch: 7 }
        );

        let excessive = grpc_dependencies(|_| {
            protobuf_ok(&QueryListClaimRecipientsResponse {
                entries: (0..=MAX_CLAIM_RECIPIENT_ENTRIES)
                    .map(|epoch| ClaimRecipientEntry {
                        epoch: epoch as u64,
                        recipient: "unused".to_string(),
                    })
                    .collect(),
            })
        });
        assert_eq!(
            query_claim_recipient(excessive.as_ref(), &host, 7, &deal).unwrap_err(),
            GonkaQueryError::TooManyClaimRecipients {
                actual: MAX_CLAIM_RECIPIENT_ENTRIES + 1,
                maximum: MAX_CLAIM_RECIPIENT_ENTRIES,
            }
        );
    }

    #[test]
    fn wrong_or_invalid_claim_recipient_is_rejected() {
        let api = MockApi::default();
        let host = api.addr_make("host");
        let deal = api.addr_make("deal");
        let other = api.addr_make("other");
        let other_string = other.to_string();
        let wrong = grpc_dependencies(move |_| {
            protobuf_ok(&QueryListClaimRecipientsResponse {
                entries: vec![ClaimRecipientEntry {
                    epoch: 7,
                    recipient: other_string.clone(),
                }],
            })
        });
        assert_eq!(
            query_claim_recipient(wrong.as_ref(), &host, 7, &deal).unwrap_err(),
            GonkaQueryError::IdentityMismatch {
                route: LIST_CLAIM_RECIPIENTS_PATH,
                field: "recipient",
            }
        );

        let invalid = grpc_dependencies(|_| {
            protobuf_ok(&QueryListClaimRecipientsResponse {
                entries: vec![ClaimRecipientEntry {
                    epoch: 7,
                    recipient: "not-an-address".to_string(),
                }],
            })
        });
        assert_eq!(
            query_claim_recipient(invalid.as_ref(), &host, 7, &deal).unwrap_err(),
            GonkaQueryError::InvalidAddress {
                route: LIST_CLAIM_RECIPIENTS_PATH,
                field: "recipient",
            }
        );
    }

    fn performance_response(
        epoch: u64,
        participant_id: String,
        earned: u64,
        rewarded: u64,
        claimed: bool,
    ) -> QueryEpochPerformanceSummaryByParticipantResponse {
        QueryEpochPerformanceSummaryByParticipantResponse {
            epoch_performance_summary: Some(EpochPerformanceSummary {
                epoch_index: epoch,
                participant_id,
                earned_coins: earned,
                rewarded_coins: rewarded,
                claimed,
                ..EpochPerformanceSummary::default()
            }),
        }
    }

    #[test]
    fn performance_preserves_components_and_adds_total_as_uint128() {
        let api = MockApi::default();
        let host = api.addr_make("host");
        let expected_host = host.to_string();
        let response_host = expected_host.clone();
        let deps = grpc_dependencies(move |request| {
            assert_eq!(request.path, EPOCH_PERFORMANCE_PATH);
            let decoded =
                QueryEpochPerformanceSummaryByParticipantRequest::decode(request.data.as_slice())
                    .unwrap();
            assert_eq!(decoded.epoch_index, 7);
            assert_eq!(decoded.participant_id, expected_host);
            protobuf_ok(&performance_response(
                7,
                response_host.clone(),
                u64::MAX,
                13,
                true,
            ))
        });

        assert_eq!(
            query_epoch_performance(deps.as_ref(), &host, 7).unwrap(),
            ParticipantPerformance {
                claimed: true,
                work_amount: Uint128::from(u64::MAX),
                reward_amount: Uint128::new(13),
                total_claim_amount: Uint128::from(u64::MAX) + Uint128::new(13),
            }
        );
    }

    #[test]
    fn missing_or_mismatched_performance_summary_is_rejected() {
        let api = MockApi::default();
        let host = api.addr_make("host");
        let missing = grpc_dependencies(|_| {
            protobuf_ok(&QueryEpochPerformanceSummaryByParticipantResponse {
                epoch_performance_summary: None,
            })
        });
        assert_eq!(
            query_epoch_performance(missing.as_ref(), &host, 7).unwrap_err(),
            GonkaQueryError::MissingField {
                route: EPOCH_PERFORMANCE_PATH,
                field: "epoch_performance_summary",
            }
        );

        let host_string = host.to_string();
        let wrong_epoch = grpc_dependencies(move |_| {
            protobuf_ok(&performance_response(8, host_string.clone(), 1, 2, false))
        });
        assert_eq!(
            query_epoch_performance(wrong_epoch.as_ref(), &host, 7).unwrap_err(),
            GonkaQueryError::IdentityMismatch {
                route: EPOCH_PERFORMANCE_PATH,
                field: "epoch_index",
            }
        );

        let other = api.addr_make("other").to_string();
        let wrong_participant = grpc_dependencies(move |_| {
            protobuf_ok(&performance_response(7, other.clone(), 1, 2, false))
        });
        assert_eq!(
            query_epoch_performance(wrong_participant.as_ref(), &host, 7).unwrap_err(),
            GonkaQueryError::IdentityMismatch {
                route: EPOCH_PERFORMANCE_PATH,
                field: "participant_id",
            }
        );

        let invalid_participant = grpc_dependencies(|_| {
            protobuf_ok(&performance_response(
                7,
                "invalid-address".to_string(),
                1,
                2,
                false,
            ))
        });
        assert_eq!(
            query_epoch_performance(invalid_participant.as_ref(), &host, 7).unwrap_err(),
            GonkaQueryError::InvalidAddress {
                route: EPOCH_PERFORMANCE_PATH,
                field: "participant_id",
            }
        );
    }

    #[test]
    fn total_vesting_accepts_empty_or_one_ngonka_coin() {
        let api = MockApi::default();
        let deal = api.addr_make("deal");
        let expected_deal = deal.to_string();
        let empty = grpc_dependencies(move |request| {
            assert_eq!(request.path, TOTAL_VESTING_AMOUNT_PATH);
            let decoded = QueryTotalVestingAmountRequest::decode(request.data.as_slice()).unwrap();
            assert_eq!(decoded.participant_address, expected_deal);
            protobuf_ok(&QueryTotalVestingAmountResponse {
                total_amount: vec![],
            })
        });
        assert_eq!(
            query_total_vesting(&empty.as_ref().querier, &deal).unwrap(),
            Uint128::zero()
        );

        let amount = grpc_dependencies(|_| {
            protobuf_ok(&QueryTotalVestingAmountResponse {
                total_amount: vec![Coin {
                    denom: NGONKA_DENOM.to_string(),
                    amount: Uint128::MAX.to_string(),
                }],
            })
        });
        assert_eq!(
            query_total_vesting(&amount.as_ref().querier, &deal).unwrap(),
            Uint128::MAX
        );
    }

    #[test]
    fn total_vesting_rejects_unexpected_duplicate_or_invalid_coins() {
        let api = MockApi::default();
        let deal = api.addr_make("deal");
        let unexpected = grpc_dependencies(|_| {
            protobuf_ok(&QueryTotalVestingAmountResponse {
                total_amount: vec![Coin {
                    denom: "uatom".to_string(),
                    amount: "1".to_string(),
                }],
            })
        });
        assert_eq!(
            query_total_vesting(&unexpected.as_ref().querier, &deal).unwrap_err(),
            GonkaQueryError::UnexpectedDenom {
                route: TOTAL_VESTING_AMOUNT_PATH
            }
        );

        let duplicate = grpc_dependencies(|_| {
            protobuf_ok(&QueryTotalVestingAmountResponse {
                total_amount: vec![
                    Coin {
                        denom: NGONKA_DENOM.to_string(),
                        amount: "1".to_string(),
                    },
                    Coin {
                        denom: NGONKA_DENOM.to_string(),
                        amount: "2".to_string(),
                    },
                ],
            })
        });
        assert_eq!(
            query_total_vesting(&duplicate.as_ref().querier, &deal).unwrap_err(),
            GonkaQueryError::DuplicateDenom {
                route: TOTAL_VESTING_AMOUNT_PATH
            }
        );

        for invalid_amount in [
            "",
            "-1",
            "not-a-number",
            "340282366920938463463374607431768211456",
        ] {
            let invalid_amount = invalid_amount.to_string();
            let invalid = grpc_dependencies(move |_| {
                protobuf_ok(&QueryTotalVestingAmountResponse {
                    total_amount: vec![Coin {
                        denom: NGONKA_DENOM.to_string(),
                        amount: invalid_amount.clone(),
                    }],
                })
            });
            assert_eq!(
                query_total_vesting(&invalid.as_ref().querier, &deal).unwrap_err(),
                GonkaQueryError::InvalidAmount {
                    route: TOTAL_VESTING_AMOUNT_PATH
                }
            );
        }
    }

    #[test]
    fn native_balance_uses_standard_bank_query_for_ngonka() {
        let mut deps = cosmwasm_std::testing::mock_dependencies();
        let deal = deps.api.addr_make("deal");
        deps.querier
            .bank
            .update_balance(deal.to_string(), vec![coin(55, NGONKA_DENOM)]);

        assert_eq!(
            query_ngonka_balance(&deps.as_ref().querier, &deal).unwrap(),
            Uint128::new(55)
        );
    }

    #[test]
    fn native_balance_query_failure_is_fail_closed() {
        struct FailingQuerier;

        impl Querier for FailingQuerier {
            fn raw_query(&self, _request: &[u8]) -> QuerierResult {
                SystemResult::Err(SystemError::UnsupportedRequest {
                    kind: "bank unavailable".to_string(),
                })
            }
        }

        let querier = FailingQuerier;
        let wrapper = QuerierWrapper::new(&querier);
        let deal = MockApi::default().addr_make("deal");

        assert_eq!(
            query_ngonka_balance(&wrapper, &deal).unwrap_err(),
            GonkaQueryError::QueryFailed {
                route: BANK_BALANCE_ROUTE
            }
        );
    }

    #[test]
    fn checked_amount_addition_reports_overflow() {
        assert_eq!(
            checked_add_amounts(Uint128::MAX, Uint128::one(), "test sum").unwrap_err(),
            GonkaQueryError::ArithmeticOverflow {
                operation: "test sum"
            }
        );
    }
}
