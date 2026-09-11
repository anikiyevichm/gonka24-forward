//! Narrow, fail-closed Gonka query boundary shared by Factory and Deal.

use cosmwasm_std::{Binary, QuerierWrapper};
use gonka_proto::{QueryGetCurrentEpochRequest, QueryGetCurrentEpochResponse};
use prost::Message;

use crate::error::GonkaQueryError;

pub const GET_CURRENT_EPOCH_PATH: &str = "/inference.inference.Query/GetCurrentEpoch";
pub const MAX_GRPC_RESPONSE_BYTES: usize = 32 * 1024;

pub fn query_current_epoch(querier: &QuerierWrapper) -> Result<u64, GonkaQueryError> {
    let request = QueryGetCurrentEpochRequest {};
    let mut request_bytes = Vec::new();
    request
        .encode(&mut request_bytes)
        .map_err(|_| GonkaQueryError::EncodeFailed {
            route: GET_CURRENT_EPOCH_PATH,
        })?;

    let response_bytes = querier
        .query_grpc(
            GET_CURRENT_EPOCH_PATH.to_string(),
            Binary::from(request_bytes),
        )
        .map_err(|_| GonkaQueryError::QueryFailed {
            route: GET_CURRENT_EPOCH_PATH,
        })?;
    if response_bytes.len() > MAX_GRPC_RESPONSE_BYTES {
        return Err(GonkaQueryError::ResponseTooLarge {
            route: GET_CURRENT_EPOCH_PATH,
            actual: response_bytes.len(),
            maximum: MAX_GRPC_RESPONSE_BYTES,
        });
    }

    QueryGetCurrentEpochResponse::decode(response_bytes.as_slice())
        .map(|response| response.epoch)
        .map_err(|_| GonkaQueryError::DecodeFailed {
            route: GET_CURRENT_EPOCH_PATH,
        })
}

#[cfg(test)]
mod tests {
    use std::{marker::PhantomData, sync::Arc};

    use cosmwasm_std::{
        from_json,
        testing::{MockApi, MockQuerier, MockStorage},
        ContractResult, Empty, GrpcQuery, OwnedDeps, Querier, QuerierResult, QueryRequest,
        SystemError, SystemResult,
    };

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
    fn current_epoch_uses_exact_route_and_empty_request() {
        let deps = grpc_dependencies(|request| {
            assert_eq!(request.path, GET_CURRENT_EPOCH_PATH);
            assert_eq!(
                QueryGetCurrentEpochRequest::decode(request.data.as_slice()).unwrap(),
                QueryGetCurrentEpochRequest {}
            );
            protobuf_ok(&QueryGetCurrentEpochResponse { epoch: 17 })
        });

        assert_eq!(query_current_epoch(&deps.as_ref().querier).unwrap(), 17);
    }

    #[test]
    fn query_failure_is_fail_closed_without_node_error_text() {
        let deps = grpc_dependencies(|_| {
            SystemResult::Err(SystemError::UnsupportedRequest {
                kind: "secret node detail".to_string(),
            })
        });

        let error = query_current_epoch(&deps.as_ref().querier).unwrap_err();
        assert_eq!(
            error,
            GonkaQueryError::QueryFailed {
                route: GET_CURRENT_EPOCH_PATH
            }
        );
        assert!(!error.to_string().contains("secret node detail"));
    }

    #[test]
    fn malformed_and_oversized_responses_are_rejected_before_use() {
        let malformed =
            grpc_dependencies(|_| SystemResult::Ok(ContractResult::Ok(Binary::from(vec![0xff]))));
        assert_eq!(
            query_current_epoch(&malformed.as_ref().querier).unwrap_err(),
            GonkaQueryError::DecodeFailed {
                route: GET_CURRENT_EPOCH_PATH
            }
        );

        let oversized = grpc_dependencies(|_| {
            SystemResult::Ok(ContractResult::Ok(Binary::from(vec![
                0;
                MAX_GRPC_RESPONSE_BYTES
                    + 1
            ])))
        });
        assert_eq!(
            query_current_epoch(&oversized.as_ref().querier).unwrap_err(),
            GonkaQueryError::ResponseTooLarge {
                route: GET_CURRENT_EPOCH_PATH,
                actual: MAX_GRPC_RESPONSE_BYTES + 1,
                maximum: MAX_GRPC_RESPONSE_BYTES,
            }
        );
    }
}
