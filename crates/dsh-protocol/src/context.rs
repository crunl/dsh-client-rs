//! 上下文占用与缓存命中率。
//!
//! 公式与官方实现一一对应（`dsh-token-meter` 的 projection 定义 +
//! `dsh-client-ui-conversation/src/client/context-occupancy.ts` +
//! `dsh-experimental-client-ui-stats/src/client/stats-usage.ts`），
//! 这里只做纯计算，不发请求。
//!
//! **重要**：`contextPressure` 在官方类型里标了
//! `SessionProjectionSummaryExcluded`——它不在 `session/list` 的投影摘要里，
//! 只能通过 `session/projections` 单独读（见 `docs/PROTOCOL.md`）。

use serde::Deserialize;

/// 上下文占用的可显示读数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occupancy {
    /// 已用 token（分子）。
    pub used: u64,
    /// 模型上下文窗口（分母）。
    pub window: u64,
    /// 百分比，`min(100, round(used / window * 100))`。
    pub percent: u8,
}

/// `contextPressure` 投影。
///
/// 各字段是**不同时刻**的 last-wins 记录，切模型时可能「新容量 + 旧压力」短暂并存；
/// 这是官方明确接受的取舍（显示用参考值，不是计费或门禁输入）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPressure {
    /// 最近一次请求由提供方上报的提示长度：未命中输入 + 缓存读 + 缓存写。
    #[serde(default)]
    pub pressure_tokens: Option<u64>,
    /// 下一次请求的预估代价：`pressure_tokens` 加上此后表面的增量估算。
    #[serde(default)]
    pub projected_tokens: Option<u64>,
    /// 最近记录的模型容量；适配器没声明时缺失。
    #[serde(default)]
    pub context_window: Option<u64>,
}

impl ContextPressure {
    /// 可显示的占用：分子取 `projectedTokens` → `pressureTokens`，分母缺一不可。
    pub fn occupancy(&self) -> Option<Occupancy> {
        let used = self.projected_tokens.or(self.pressure_tokens)?;
        let window = self.context_window?;
        if window == 0 {
            return None;
        }
        // 官方用 Math.round，等价于「四舍五入」；比例本身先夹到 1。
        let ratio = (used as f64 / window as f64).min(1.0);
        Some(Occupancy {
            used,
            window,
            percent: (ratio * 100.0).round() as u8,
        })
    }
}

/// `tokenUsage` 投影：整份会话日志累计的提供方用量。
///
/// 四个桶互不重叠——推理 token 已经算在 `output_tokens` 里，不再重复累计。
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    /// 未命中缓存的输入 token。
    #[serde(default)]
    pub uncached_input_tokens: u64,
    /// 输出 token（含推理）。
    #[serde(default)]
    pub output_tokens: u64,
    /// 命中缓存的读 token。
    #[serde(default)]
    pub cache_read_tokens: u64,
    /// 写入缓存的 token。
    #[serde(default)]
    pub cache_write_tokens: u64,
}

impl TokenUsage {
    /// 提示长度：输入、缓存读、缓存写三者不重叠，直接相加。
    pub fn context_tokens(&self) -> u64 {
        self.uncached_input_tokens + self.cache_read_tokens + self.cache_write_tokens
    }

    /// 缓存命中率百分比，`min(1, cacheRead / context)`。
    ///
    /// 没有任何输入时返回 `None`——分母为 0 时不谎报 0%。
    pub fn cache_hit_percent(&self) -> Option<u8> {
        let context = self.context_tokens();
        if context == 0 {
            return None;
        }
        let ratio = (self.cache_read_tokens as f64 / context as f64).min(1.0);
        Some((ratio * 100.0).round() as u8)
    }
}

/// `session/projections` 的返回值里我们用到的部分。
#[derive(Debug, Clone, Deserialize)]
pub struct SessionProjectionsValue {
    /// 已知投影值；键缺失即该投影还没产生。
    #[serde(default)]
    pub values: ProjectionValues,
}

/// 单个会话的投影值集合。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionValues {
    /// 上下文压力。
    #[serde(default)]
    pub context_pressure: Option<ContextPressure>,
    /// 累计用量。
    #[serde(default)]
    pub token_usage: Option<TokenUsage>,
}
