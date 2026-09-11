//! Stateless calculations and validated native-query helpers shared by the
//! Factory and Deal contracts.
//!
//! This package owns no storage, authorization, state transitions, transfers,
//! or transaction construction. Cargo links the used functions into each
//! contract Wasm; it is not deployed as a third contract.

pub mod error;
pub mod gonka;
pub mod math;
