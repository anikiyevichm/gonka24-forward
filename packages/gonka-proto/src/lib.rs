//! Generated protobuf bindings for the pinned Gonka protocol source.
//!
//! Only the wire types required by the marketplace are re-exported. Semantic
//! validation belongs to the Deal contract, not to this crate.

mod generated;

pub use generated::cosmos::base::v1beta1::Coin;
pub use generated::inference::inference::{
    ClaimRecipientEntry, EpochPerformanceSummary, QueryEpochPerformanceSummaryByParticipantRequest,
    QueryEpochPerformanceSummaryByParticipantResponse, QueryGetCurrentEpochRequest,
    QueryGetCurrentEpochResponse, QueryListClaimRecipientsRequest,
    QueryListClaimRecipientsResponse,
};
pub use generated::inference::streamvesting::{
    QueryTotalVestingAmountRequest, QueryTotalVestingAmountResponse,
};
pub use generated::provenance::{SOURCE_COMMIT, SOURCE_REPOSITORY};
