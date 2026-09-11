use cosmwasm_schema::write_api;
use marketplace_api::deal::{ExecuteMsg, InstantiateMsg, QueryMsg};
use std::{fs, path::Path};

fn main() {
    std::env::set_current_dir(env!("CARGO_MANIFEST_DIR"))
        .expect("failed to enter Deal crate directory");

    reset_schema_directory();

    write_api! {
        name: "marketplace-deal",
        instantiate: InstantiateMsg,
        execute: ExecuteMsg,
        query: QueryMsg,
    }
}

fn reset_schema_directory() {
    let schema_dir = Path::new("schema");
    let raw_schema_dir = schema_dir.join("raw");

    if raw_schema_dir.exists() {
        fs::remove_dir_all(raw_schema_dir).expect("failed to remove stale raw Deal schemas");
    }

    if !schema_dir.exists() {
        return;
    }

    for entry in fs::read_dir(schema_dir).expect("failed to read Deal schema directory") {
        let path = entry.expect("failed to read Deal schema entry").path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            fs::remove_file(path).expect("failed to remove stale Deal schema");
        }
    }
}
