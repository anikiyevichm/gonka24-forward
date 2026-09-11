use gonka_proto::{
    ClaimRecipientEntry, Coin, EpochPerformanceSummary,
    QueryEpochPerformanceSummaryByParticipantRequest,
    QueryEpochPerformanceSummaryByParticipantResponse, QueryGetCurrentEpochRequest,
    QueryGetCurrentEpochResponse, QueryListClaimRecipientsRequest,
    QueryListClaimRecipientsResponse, QueryTotalVestingAmountRequest,
    QueryTotalVestingAmountResponse,
};
use prost::Message;

// These bytes were produced with the vendored protoc 3.2.0 binary from the
// pinned Gonka .proto snapshot, independently of the generated Rust structs.
// They make protobuf field-number drift visible in ordinary `cargo test`.

#[test]
fn current_epoch_wire_format_matches_protoc() {
    assert_eq!(QueryGetCurrentEpochRequest {}.encode_to_vec(), []);
    assert_eq!(
        QueryGetCurrentEpochResponse { epoch: 42 }.encode_to_vec(),
        [0x08, 0x2a]
    );

    let decoded = QueryGetCurrentEpochResponse::decode([0x08, 0x2a].as_slice()).unwrap();
    assert_eq!(decoded.epoch, 42);
}

#[test]
fn claim_recipient_wire_format_matches_protoc() {
    let request = QueryListClaimRecipientsRequest {
        participant: "gonka1host".to_string(),
    };
    assert_eq!(
        request.encode_to_vec(),
        [0x0a, 0x0a, b'g', b'o', b'n', b'k', b'a', b'1', b'h', b'o', b's', b't']
    );

    let response = QueryListClaimRecipientsResponse {
        entries: vec![ClaimRecipientEntry {
            epoch: 7,
            recipient: "gonka1deal".to_string(),
        }],
    };
    let golden = [
        0x0a, 0x0e, 0x08, 0x07, 0x12, 0x0a, b'g', b'o', b'n', b'k', b'a', b'1', b'd', b'e', b'a',
        b'l',
    ];
    assert_eq!(response.encode_to_vec(), golden);
    assert_eq!(
        QueryListClaimRecipientsResponse::decode(golden.as_slice()).unwrap(),
        response
    );
}

#[test]
fn epoch_performance_wire_format_matches_protoc() {
    let request = QueryEpochPerformanceSummaryByParticipantRequest {
        epoch_index: 7,
        participant_id: "gonka1host".to_string(),
    };
    assert_eq!(
        request.encode_to_vec(),
        [0x08, 0x07, 0x12, 0x0a, b'g', b'o', b'n', b'k', b'a', b'1', b'h', b'o', b's', b't',]
    );

    let response = QueryEpochPerformanceSummaryByParticipantResponse {
        epoch_performance_summary: Some(EpochPerformanceSummary {
            epoch_index: 7,
            participant_id: "gonka1host".to_string(),
            earned_coins: 11,
            rewarded_coins: 13,
            claimed: true,
            ..EpochPerformanceSummary::default()
        }),
    };
    let golden = [
        0x0a, 0x14, 0x08, 0x07, 0x12, 0x0a, b'g', b'o', b'n', b'k', b'a', b'1', b'h', b'o', b's',
        b't', 0x28, 0x0b, 0x30, 0x0d, 0x50, 0x01,
    ];
    assert_eq!(response.encode_to_vec(), golden);
    assert_eq!(
        QueryEpochPerformanceSummaryByParticipantResponse::decode(golden.as_slice()).unwrap(),
        response
    );
}

#[test]
fn total_vesting_wire_format_matches_protoc() {
    let request = QueryTotalVestingAmountRequest {
        participant_address: "gonka1deal".to_string(),
    };
    assert_eq!(
        request.encode_to_vec(),
        [0x0a, 0x0a, b'g', b'o', b'n', b'k', b'a', b'1', b'd', b'e', b'a', b'l']
    );

    let response = QueryTotalVestingAmountResponse {
        total_amount: vec![Coin {
            denom: "ngonka".to_string(),
            amount: "123".to_string(),
        }],
    };
    let golden = [
        0x0a, 0x0d, 0x0a, 0x06, b'n', b'g', b'o', b'n', b'k', b'a', 0x12, 0x03, b'1', b'2', b'3',
    ];
    assert_eq!(response.encode_to_vec(), golden);
    assert_eq!(
        QueryTotalVestingAmountResponse::decode(golden.as_slice()).unwrap(),
        response
    );
}
