//! Test-only Wasm ABI probe. Uses the real pinned ExternalQuerier via Deps.
use cosmwasm_std::{entry_point, to_json_binary, Binary, Deps, Env, StdResult};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct ProbeMsg {
    pub request: Binary,
}

#[entry_point]
pub fn query(deps: Deps, _env: Env, msg: ProbeMsg) -> StdResult<Binary> {
    to_json_binary(&deps.querier.raw_query(msg.request.as_slice()))
}
