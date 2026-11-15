use common_context::util::configuration::EnvironmentTree;
use serde_json::json;
use std::collections::HashMap;

#[test]
fn overlays_arbitrary_map_keys_and_arrays_without_application_field_names() {
    let base = json!({"custom":{"eip155:1":{"items":[{"enabled":false}]}}});
    let input = HashMap::from([(
        "OTHER__CUSTOM__EIP155:1__ITEMS__0__ENABLED".to_owned(),
        "true".to_owned(),
    )]);
    let merged = EnvironmentTree::merge(base, "OTHER__", input).unwrap();
    assert_eq!(merged["custom"]["eip155:1"]["items"][0]["enabled"], "true");
    let bad = HashMap::from([("OTHER__ITEMS__999999999999__NAME".to_owned(), "x".to_owned())]);
    assert!(EnvironmentTree::merge(json!({"items":[]}), "OTHER__", bad).is_err());
}
