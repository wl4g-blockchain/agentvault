use std::collections::HashSet;

use crate::tracing_sampler::TracingSampleRule;

#[test]
fn test_rule() {
    let rule = TracingSampleRule {
        protocol: "http".to_string(),
        request_types: HashSet::new(),
        ratio: 1.0,
    };
    assert_eq!(rule.match_rule("not_http", None), None);
    assert_eq!(rule.match_rule("http", None), Some(1.0));
    assert_eq!(rule.match_rule("http", Some("abc")), Some(1.0));
    let rule1 = TracingSampleRule {
        protocol: "http".to_string(),
        request_types: HashSet::from(["mysql".to_string()]),
        ratio: 1.0,
    };
    assert_eq!(rule1.match_rule("http", None), None);
    assert_eq!(rule1.match_rule("http", Some("abc")), None);
    assert_eq!(rule1.match_rule("http", Some("mysql")), Some(1.0));
}
