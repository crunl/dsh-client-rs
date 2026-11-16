//! 上下文占用与缓存命中率的公式（与官方实现逐条对齐）。

use dsh_protocol::context::{ContextPressure, SessionProjectionsValue, TokenUsage};

fn pressure(json: &str) -> ContextPressure {
    serde_json::from_str(json).expect("测试里的 expect 是正当的")
}

#[test]
fn occupancy_prefers_projected_tokens() {
    let pressure =
        pressure(r#"{"contextWindow":1048576,"pressureTokens":14731,"projectedTokens":15218}"#);
    let occupancy = pressure.occupancy().expect("两个字段都在");
    assert_eq!(occupancy.used, 15218);
    assert_eq!(occupancy.window, 1048576);
    assert_eq!(occupancy.percent, 1);
}

#[test]
fn occupancy_falls_back_to_pressure_tokens() {
    let pressure = pressure(r#"{"contextWindow":1000,"pressureTokens":615}"#);
    let occupancy = pressure.occupancy().expect("pressureTokens 够用");
    assert_eq!(occupancy.used, 615);
    assert_eq!(occupancy.percent, 62);
}

#[test]
fn occupancy_needs_both_sides() {
    assert!(pressure(r#"{"pressureTokens":10}"#).occupancy().is_none());
    assert!(pressure(r#"{"contextWindow":10}"#).occupancy().is_none());
    assert!(
        pressure(r#"{"contextWindow":0,"pressureTokens":10}"#)
            .occupancy()
            .is_none()
    );
}

#[test]
fn occupancy_is_clamped_at_100() {
    let pressure = pressure(r#"{"contextWindow":100,"projectedTokens":250}"#);
    assert_eq!(pressure.occupancy().expect("够用").percent, 100);
}

#[test]
fn cache_hit_rate_matches_official_formula() {
    let usage: TokenUsage = serde_json::from_str(
        r#"{"uncachedInputTokens":5899,"outputTokens":418,"cacheReadTokens":8832,"cacheWriteTokens":0}"#,
    )
    .expect("测试 fixture");
    // 与 pressureTokens 自洽：5899 + 8832 + 0 = 14731
    assert_eq!(usage.context_tokens(), 14731);
    assert_eq!(usage.cache_hit_percent(), Some(60));
}

#[test]
fn cache_hit_rate_without_input_is_unknown() {
    let usage = TokenUsage::default();
    assert_eq!(usage.cache_hit_percent(), None);
}

#[test]
fn projections_value_parses_real_shape() {
    let value: SessionProjectionsValue = serde_json::from_str(
        r#"{"kind":"sequenced","asOfSeq":18,"values":{
            "contextPressure":{"contextWindow":1048576,"pressureTokens":14731,"projectedTokens":15218},
            "tokenUsage":{"cacheReadTokens":8832,"cacheWriteTokens":0,"outputTokens":418,"uncachedInputTokens":5899}
        }}"#,
    )
    .expect("真实响应形状");
    assert_eq!(
        value
            .values
            .context_pressure
            .and_then(|p| p.occupancy())
            .map(|o| o.percent),
        Some(1)
    );
    assert_eq!(
        value.values.token_usage.and_then(|u| u.cache_hit_percent()),
        Some(60)
    );
}
