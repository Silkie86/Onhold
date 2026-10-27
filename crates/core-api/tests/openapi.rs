//! The committed OpenAPI document (which the TypeScript client is generated
//! from) must match what the handlers actually serve.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

#[test]
fn the_committed_spec_is_current() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../web/packages/api-client/openapi.json");
    let committed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let current = serde_json::to_value(core_api::openapi()).unwrap();
    assert!(
        committed == current,
        "the API changed; run `cargo run -p onhold -- openapi > web/packages/api-client/openapi.json` \
         and `pnpm --dir web generate`, then commit both"
    );
}

#[test]
fn every_operation_is_documented() {
    let spec = serde_json::to_value(core_api::openapi()).unwrap();
    for (path, item) in spec["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            assert!(
                op["tags"].as_array().is_some_and(|t| !t.is_empty()),
                "{method} {path} has no tag"
            );
            assert!(
                op["responses"].as_object().is_some_and(|r| !r.is_empty()),
                "{method} {path} has no responses"
            );
            assert!(
                op["operationId"].is_string(),
                "{method} {path} has no operationId"
            );
        }
    }
}
