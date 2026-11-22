//! 聊天呈现的纯函数规划层。
//!
//! 这一层只回答一个问题：给定一组可绘制行与用户的详略偏好，本次绘制应该出现
//! 哪些折叠控件、每个控件默认开还是合。
//!
//! 三条第一性约定：
//! 1. 轮次边界来自用户输入行（官方 `exchange` 的定义），既不依赖 turn 事件，
//!    也不依赖数组下标——本地乐观消息和 `Seq` 身份同样成立；
//! 2. 折叠身份是语义键，与渲染位置无关，分页 / 流式追加都不改变它；
//! 3. 档位是两个决策的组合，不是一个序数：「已完成轮是否收起」（官方
//!    `foldCompletedTurns`）与「展开到第几层容器」。前者按档位显式给，后者
//!    才用深度标量——这样紧凑/标准/详细都能折叠历史轮，而完全展开不折。
//!
//! 本模块无 GPUI、无 IO、无订阅。输入只有行切片与偏好，输出只有绘制指令；
//! 因此可以用表驱动测试穷举 `(档位 × 运行态 × 覆盖)`。

use std::collections::HashMap;
use std::ops::Range;

use crate::timeline::{RenderId, TimelineRow};
use crate::transcript::ToolStatus;

/// 详略档位。两个正交决策的合成：
/// - `fold_completed`：已完成轮次是否默认整轮收起（官方 `foldCompletedTurns`）；
/// - `depth`：展开到第几层容器（Turn → Group → 单节点）。
///
/// 官方 compact / standard / detailed 三档都折叠已完成轮次，只有 verbose 不折；
/// 把这条从深度里拎出来，才能让相邻档的差异仍落在「一层内容出现或消失」上。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Detail {
    /// 折叠整轮；展开后只到组头，组头给类别摘要，推理不给预览。
    Quiet,
    /// 折叠整轮；展开后只到组头，但组头带实时详情、推理给首行预览。
    #[default]
    Brief,
    /// 折叠整轮；展开后过程组也展开，看得到每条推理与工具行。
    Standard,
    /// 不折叠整轮、完全平铺，连单节点正文也默认展开。
    Full,
}

impl Detail {
    /// settings 页按这个顺序画。
    pub const ALL: [Self; 4] = [Self::Quiet, Self::Brief, Self::Standard, Self::Full];

    /// 默认展开到第几层容器：`0`=整轮、`1`=过程组、`2`=单节点、`3`=全部。
    /// 注意整轮的默认开合不走这里，而走 [`Self::fold_completed`]。
    const fn depth(self) -> u8 {
        match self {
            Self::Quiet => 0,
            Self::Brief => 1,
            Self::Standard => 2,
            Self::Full => 3,
        }
    }

    /// 已完成轮次默认是否整轮收起（官方 `foldCompletedTurns`）。
    /// compact / standard / detailed 为真，verbose 为假——与深度无关。
    const fn fold_completed(self) -> bool {
        !self.is_flat()
    }

    /// Full 档不生成任何包裹控件，完全平铺。
    const fn is_flat(self) -> bool {
        matches!(self, Self::Full)
    }

    /// 该档下第 `level` 层容器（0=Turn，1=Group）默认是否展开。
    const fn layer_open(self, level: u8) -> bool {
        level < self.depth()
    }

    /// 组头是否带运行中实时详情（compact 档不给）。
    const fn live_detail(self) -> bool {
        self.depth() >= 1
    }
}

/// 一个可折叠容器的语义身份。与渲染位置 / 数组下标无关。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FoldKey {
    /// 整轮：锚点是开启该轮的用户输入行（无输入时取窗口首行）。
    Turn(String),
    /// 过程组：整轮锚 + 组内序号。
    Group { turn: String, ordinal: u16 },
    /// 单节点正文：reasoning / tool 行的稳定 id。
    Node(String),
}

/// 容器层级。整轮包过程组；单节点正文的开合由渲染层按档位管理，不进规划树。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldLevel {
    Turn,
    Group,
}

/// 一个折叠容器的本次绘制规格。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldSpec {
    pub key: FoldKey,
    pub level: FoldLevel,
    /// 是否真的可折叠：运行中 / 失败的容器恒展开，不给收起动作。
    pub collapsible: bool,
    /// 本次绘制的开合（已考虑档位默认值与用户覆盖）。
    pub open: bool,
    /// 折叠态手柄文案（组头类别统计 / 整轮摘要）。
    pub summary: String,
}

/// 会话内的交互状态：语义键 → 手动开合覆盖。
///
/// 取代原来的三个 `HashMap<String, bool>` 与 `debug_tools_open`。键是语义身份，
/// 分页或流式追加改变行位置时不会漂移。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    pub folds: HashMap<FoldKey, bool>,
    /// 调试：全部强制展开（原 `DSH_DEBUG_TOOL_OPEN`）。
    pub force_open: bool,
}

impl Overrides {
    /// 某个折叠容器本次是否展开：调试开关 > 手动覆盖 > 档位默认。
    pub fn is_open(&self, key: &FoldKey, default_open: bool) -> bool {
        self.force_open || self.folds.get(key).copied().unwrap_or(default_open)
    }

    /// 记录一次手动切换。
    pub fn set(&mut self, key: FoldKey, open: bool) {
        self.folds.insert(key, open);
    }

    /// 某一层的当前开合，供单节点正文（reasoning / tool）复用同一份覆盖。
    pub fn node_open(&self, id: &str, default_open: bool) -> bool {
        self.force_open
            || self
                .folds
                .get(&FoldKey::Node(id.to_owned()))
                .copied()
                .unwrap_or(default_open)
    }

    /// 切换单节点正文；返回切换后的状态。
    pub fn toggle_node(&mut self, id: &str, default_open: bool) -> bool {
        let key = FoldKey::Node(id.to_owned());
        let next = !self.is_open(&key, default_open);
        self.folds.insert(key, next);
        next
    }
}

/// 一次绘制的原子指令。渲染层只做机械绘制，不再判断详略策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderOp<'a> {
    /// 原样绘制一行。
    Row(&'a TimelineRow),
    /// 一个可折叠的过程组（连续的 reasoning / tool 行）。
    Group {
        spec: FoldSpec,
        rows: &'a [TimelineRow],
    },
    /// 一个整轮折叠窗口。折叠时只画 `inner` 里的 `Row`（豁免的正文回复），
    /// 跳过其中的 `Group`——这就是「整轮收起但回复仍可见」。
    Turn {
        spec: FoldSpec,
        inner: Vec<RenderOp<'a>>,
    },
}

/// 把一段行规划成本次绘制指令。纯函数，不接触 GPUI。`detail` 为空时才平铺。
///
/// `session_busy` 是运行态的门：只有会话忙、且是最后一个 exchange 时，内部的
/// running 行才让容器恒展开。历史轮里残留的 `ToolStatus::Running`（无 result 的
/// 调用、被中断的生成）不会让一轮永久不可折叠。
pub fn plan<'a>(
    rows: &'a [TimelineRow],
    detail: Detail,
    over: &Overrides,
    session_busy: bool,
) -> Vec<RenderOp<'a>> {
    if detail.is_flat() {
        return rows.iter().map(RenderOp::Row).collect();
    }
    let mut ops = Vec::with_capacity(rows.len());
    let mut start = 0usize;
    for end in exchange_bounds(rows) {
        if end > start {
            // 活跃轮 = 会话忙 且 本窗口之后不再有过程行。这比"最后一个窗口"更准：
            // 排队消息会在跑着的轮后追加一条裸 User 行，那个窗口内没有过程行，
            // 真正在跑的过程窗口反而不是最后一个。
            let active = session_busy && !rows[end..].iter().any(is_process_row);
            plan_exchange(&rows[start..end], detail, over, active, &mut ops);
        }
        start = end;
    }
    ops
}

/// 用户输入行的下标构成 exchange 边界；末尾补一个哨兵收住最后一段。
fn exchange_bounds(rows: &[TimelineRow]) -> Vec<usize> {
    let mut bounds: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| matches!(row, TimelineRow::User { .. }))
        .map(|(index, _)| index)
        .collect();
    bounds.push(rows.len());
    bounds
}

/// 规划一个 exchange 窗口：窗口内连续过程行成组，整窗（含组）成一个 Turn。
fn plan_exchange<'a>(
    window: &'a [TimelineRow],
    detail: Detail,
    over: &Overrides,
    active: bool,
    out: &mut Vec<RenderOp<'a>>,
) {
    let runs = process_runs(window);
    if runs.is_empty() {
        out.extend(window.iter().map(RenderOp::Row));
        return;
    }

    let anchor = row_label(&window[0]);
    let turn_key = FoldKey::Turn(anchor.clone());
    // active 为假（历史轮 / 会话空闲）时不认 running，但失败 / 中断仍要恒展开。
    let turn_pin = pin_of(window, active);

    let mut inner = Vec::new();
    let mut cursor = 0usize;
    for (ordinal, range) in runs.into_iter().enumerate() {
        // 组之前的正文 / 输入行：豁免行，整轮折叠时仍可见。
        for row in &window[cursor..range.start] {
            inner.push(RenderOp::Row(row));
        }
        let slice = &window[range.clone()];
        let pin = pin_of(slice, active);
        let running = pin == Pin::Running;
        let key = FoldKey::Group {
            turn: anchor.clone(),
            ordinal: ordinal as u16,
        };
        let default_open = detail.layer_open(1);
        let spec = FoldSpec {
            key: key.clone(),
            level: FoldLevel::Group,
            collapsible: !pin.is_pinned(),
            open: pin.is_pinned() || over.is_open(&key, default_open),
            summary: group_summary(slice, running, detail.live_detail()),
        };
        inner.push(RenderOp::Group { spec, rows: slice });
        cursor = range.end;
    }
    for row in &window[cursor..] {
        inner.push(RenderOp::Row(row));
    }

    // 已完成轮的默认开合由 fold_completed 决定；运行中 / 失败 / 中断优先恒展开。
    let default_open = !detail.fold_completed();
    let turn_spec = FoldSpec {
        key: turn_key.clone(),
        level: FoldLevel::Turn,
        collapsible: !turn_pin.is_pinned(),
        open: turn_pin.is_pinned() || over.is_open(&turn_key, default_open),
        summary: turn_summary(window, turn_pin),
    };
    out.push(RenderOp::Turn {
        spec: turn_spec,
        inner,
    });
}

/// 连续过程行（reasoning / tool）的区间；正文、输入、提示都会断组。
pub fn process_runs(rows: &[TimelineRow]) -> Vec<Range<usize>> {
    let mut runs = Vec::new();
    let mut index = 0usize;
    while index < rows.len() {
        if !is_process_row(&rows[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < rows.len() && is_process_row(&rows[index]) {
            index += 1;
        }
        runs.push(start..index);
    }
    runs
}

fn is_process_row(row: &TimelineRow) -> bool {
    matches!(row, TimelineRow::Reasoning { .. } | TimelineRow::Tool(_))
}

fn is_running_row(row: &TimelineRow) -> bool {
    match row {
        TimelineRow::Reasoning { streaming, .. } => *streaming,
        TimelineRow::Tool(view) => view.status == ToolStatus::Running,
        _ => false,
    }
}

/// 容器为何必须保持展开、不给收起入口。
///
/// - `Running`：只在会话忙的活跃轮认，历史残留的 Running 不算（见 `plan` 的 active）；
/// - `Failed` / `Interrupted`：工具报错或被用户中断，**历史轮也恒展开**——
///   否则用户看不到失败原因，与官方 `error/aborted/interrupted` 恒展开相反。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pin {
    None,
    Running,
    Failed,
    Interrupted,
}

impl Pin {
    fn is_pinned(self) -> bool {
        self != Self::None
    }
}

/// 一段行是否富含「不可收起」的理由；运行中需要 `active` 门，失败与中断不需要。
fn pin_of(rows: &[TimelineRow], active: bool) -> Pin {
    if active && rows.iter().any(is_running_row) {
        return Pin::Running;
    }
    if rows
        .iter()
        .any(|row| matches!(row, TimelineRow::Notice { .. }))
    {
        return Pin::Interrupted;
    }
    if rows.iter().any(|row| {
        matches!(
            row,
            TimelineRow::Tool(view)
                if matches!(view.status, ToolStatus::Error | ToolStatus::Stopped)
        )
    }) {
        return Pin::Failed;
    }
    Pin::None
}

fn row_label(row: &TimelineRow) -> String {
    match row {
        TimelineRow::User { id, .. }
        | TimelineRow::Assistant { id, .. }
        | TimelineRow::Reasoning { id, .. }
        | TimelineRow::Notice { id, .. } => id.label(),
        TimelineRow::Tool(view) => RenderId::Tool(view.id.clone()).label(),
    }
}

/// 组头文案：运行中可换最后一条工具标题；否则给类别统计 top3。
pub fn group_summary(rows: &[TimelineRow], running: bool, live_detail: bool) -> String {
    let tools: Vec<_> = rows
        .iter()
        .filter_map(|row| match row {
            TimelineRow::Tool(view) => Some(view),
            _ => None,
        })
        .collect();
    if tools.is_empty() {
        return if running { "思考中" } else { "已思考" }.to_owned();
    }
    if live_detail && running && tools.last().is_some_and(|tool| !tool.title.is_empty()) {
        return tools.last().unwrap().title.clone();
    }
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for tool in &tools {
        let kind = tool.family.category();
        match counts.iter_mut().find(|(name, _)| *name == kind) {
            Some(entry) => entry.1 += 1,
            None => counts.push((kind, 1)),
        }
    }
    counts.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    counts.truncate(3);
    counts
        .into_iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// 整轮控件文案：状态 + 工具计数。状态区分工作中 / 已中断 / 执行失败 / 已完成。
fn turn_summary(window: &[TimelineRow], pin: Pin) -> String {
    let tool_count = window
        .iter()
        .filter(|row| matches!(row, TimelineRow::Tool(_)))
        .count();
    let label = match pin {
        Pin::Running => "工作中",
        Pin::Interrupted => "已中断",
        Pin::Failed => "执行失败",
        Pin::None => "已完成",
    };
    match tool_count {
        0 => label.to_owned(),
        n => format!("{label} · {n} 次工具"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::{ToolFamily, ToolResultView, ToolView};
    use crate::transcript::MessageId;

    fn user(turn: u64) -> TimelineRow {
        TimelineRow::User {
            id: RenderId::User(MessageId::Turn { turn, step: 0 }),
            text: format!("问 {turn}"),
        }
    }
    fn assistant(turn: u64) -> TimelineRow {
        TimelineRow::Assistant {
            id: RenderId::Assistant {
                id: MessageId::Turn { turn, step: 1 },
                index: 0,
            },
            text: format!("答 {turn}"),
            streaming: false,
        }
    }
    fn reasoning(turn: u64, streaming: bool) -> TimelineRow {
        TimelineRow::Reasoning {
            id: RenderId::Reasoning {
                id: MessageId::Turn { turn, step: 1 },
                index: 0,
            },
            text: "思考".to_owned(),
            streaming,
        }
    }
    fn tool(turn: u64, running: bool) -> TimelineRow {
        TimelineRow::Tool(ToolView {
            id: format!("call-{turn}"),
            name: "bash".to_owned(),
            family: ToolFamily::Shell,
            title: format!("跑命令 {turn}"),
            summary: String::new(),
            error_summary: None,
            details: Vec::new(),
            status: if running {
                ToolStatus::Running
            } else {
                ToolStatus::Ok
            },
            output_chars: 0,
            elapsed_ms: None,
            result: ToolResultView::None,
        })
    }
    fn failed_tool(turn: u64) -> TimelineRow {
        let mut row = tool(turn, false);
        let TimelineRow::Tool(view) = &mut row else {
            unreachable!()
        };
        view.status = ToolStatus::Error;
        view.title = "命令失败".to_owned();
        view.error_summary = Some("exit 1: permission denied".to_owned());
        row
    }
    fn interrupted(turn: u64) -> TimelineRow {
        TimelineRow::Notice {
            id: RenderId::Notice(turn),
            text: "生成被中断".to_owned(),
        }
    }
    fn count_ops<'a>(
        ops: &[RenderOp<'a>],
        turn: usize,
        group: usize,
        row: usize,
    ) -> (usize, usize, usize) {
        let mut t = turn;
        let mut g = group;
        let mut r = row;
        for op in ops {
            match op {
                RenderOp::Row(_) => r += 1,
                RenderOp::Group { .. } => g += 1,
                RenderOp::Turn { inner, .. } => {
                    t += 1;
                    let (tt, gg, rr) = count_ops(inner, 0, 0, 0);
                    t += tt;
                    g += gg;
                    r += rr;
                }
            }
        }
        (t, g, r)
    }

    #[test]
    fn no_process_rows_are_all_plain_rows() {
        let rows = vec![user(1), assistant(1)];
        let ops = plan(&rows, Detail::Standard, &Overrides::default(), false);
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], RenderOp::Row(_)));
        assert!(matches!(ops[1], RenderOp::Row(_)));
    }

    #[test]
    fn one_exchange_with_process_becomes_a_turn_with_a_group() {
        let rows = vec![user(1), reasoning(1, false), tool(1, false), assistant(1)];
        let ops = plan(&rows, Detail::Standard, &Overrides::default(), false);
        assert_eq!(count_ops(&ops, 0, 0, 0), (1, 1, 2));
        let RenderOp::Turn { spec, inner } = &ops[0] else {
            panic!("应是整轮");
        };
        assert_eq!(spec.level, FoldLevel::Turn);
        // 用户输入 + 正文回复是豁免行，仍留在整轮内部。
        assert!(matches!(inner.first(), Some(RenderOp::Row(_))));
        assert!(matches!(inner.last(), Some(RenderOp::Row(_))));
    }

    #[test]
    fn each_user_input_opens_a_new_exchange() {
        let rows = vec![user(1), tool(1, false), user(2), tool(2, false)];
        let ops = plan(&rows, Detail::Standard, &Overrides::default(), false);
        assert_eq!(count_ops(&ops, 0, 0, 0), (2, 2, 2));
    }

    #[test]
    fn detail_depth_controls_default_open_per_layer() {
        let rows = vec![user(1), reasoning(1, false), tool(1, false), assistant(1)];
        // 三档都折叠已完成轮；展开后 Brief 只到组头，Standard 连组也展开。
        for (detail, turn_open, group_open) in [
            (Detail::Quiet, false, false),
            (Detail::Brief, false, false),
            (Detail::Standard, false, true),
        ] {
            let ops = plan(&rows, detail, &Overrides::default(), false);
            let RenderOp::Turn { spec: turn, inner } = &ops[0] else {
                panic!("应是整轮");
            };
            assert_eq!(turn.open, turn_open, "turn open @ {detail:?}");
            let RenderOp::Group { spec: group, .. } = inner
                .iter()
                .find(|op| matches!(op, RenderOp::Group { .. }))
                .unwrap()
            else {
                unreachable!()
            };
            assert_eq!(group.open, group_open, "group open @ {detail:?}");
        }
    }

    #[test]
    fn fold_completed_follows_official_capability_bit() {
        // 官方 compact/standard/detailed 折叠已完成轮，只有 verbose 不折。
        // 这是与「展开深度」正交的能力位，不能从深度推导。
        assert!(Detail::Quiet.fold_completed());
        assert!(Detail::Brief.fold_completed());
        assert!(Detail::Standard.fold_completed());
        assert!(!Detail::Full.fold_completed());
    }

    #[test]
    fn failed_or_interrupted_turn_is_pinned_even_in_history() {
        // 回归：失败 / 中断的轮次必须恒展开，否则用户看不到原因。
        // 会话空闲（历史轮）也成立——这与「残留 Running 不锁死折叠」相反。
        for rows in [
            vec![user(1), failed_tool(1), assistant(1)],
            vec![user(1), tool(1, false), interrupted(1), assistant(1)],
        ] {
            for detail in [Detail::Quiet, Detail::Brief, Detail::Standard] {
                let ops = plan(&rows, detail, &Overrides::default(), false);
                let RenderOp::Turn { spec, .. } = &ops[0] else {
                    panic!("应是整轮");
                };
                assert!(!spec.collapsible, "失败轮不该可收起 @ {detail:?}");
                assert!(spec.open, "失败轮必须展开 @ {detail:?}");
            }
        }
    }

    #[test]
    fn running_rows_make_containers_non_collapsible_and_open() {
        let rows = vec![user(1), tool(1, true), assistant(1)];
        let ops = plan(&rows, Detail::Quiet, &Overrides::default(), true);
        let RenderOp::Turn { spec: turn, inner } = &ops[0] else {
            panic!("应是整轮");
        };
        assert!(!turn.collapsible);
        assert!(turn.open);
        let RenderOp::Group { spec: group, .. } = inner
            .iter()
            .find(|op| matches!(op, RenderOp::Group { .. }))
            .unwrap()
        else {
            unreachable!()
        };
        assert!(!group.collapsible);
        assert!(group.open);
    }

    #[test]
    fn overrides_win_over_detail_default() {
        let rows = vec![user(1), tool(1, false), assistant(1)];
        let mut over = Overrides::default();
        let anchor = row_label(&rows[0]);
        over.set(FoldKey::Turn(anchor), true);
        let ops = plan(&rows, Detail::Quiet, &over, false);
        let RenderOp::Turn { spec, .. } = &ops[0] else {
            panic!("应是整轮");
        };
        assert!(spec.open, "手动覆盖压过 Quiet 的默认折叠");
    }

    #[test]
    fn full_detail_is_flat_with_no_wrappers() {
        let rows = vec![user(1), tool(1, false), assistant(1)];
        let ops = plan(&rows, Detail::Full, &Overrides::default(), false);
        assert!(ops.iter().all(|op| matches!(op, RenderOp::Row(_))));
        assert_eq!(count_ops(&ops, 0, 0, 0), (0, 0, 3));
    }

    #[test]
    fn stale_running_row_in_history_stays_collapsible_when_idle() {
        // 回归：历史轮里没有 result 的 tool call 会永久残留 Running。
        // 会话空闲时它必须照常可折叠，不能像运行中那样恒展开、无折叠入口。
        let rows = vec![user(1), tool(1, true), user(2), tool(2, false)];
        let ops = plan(&rows, Detail::Quiet, &Overrides::default(), false);
        let RenderOp::Turn { spec: first, .. } = &ops[0] else {
            panic!("应是整轮");
        };
        assert!(first.collapsible, "历史轮的残留 Running 行不该锁死折叠");
        assert!(!first.open, "Quiet 档历史轮默认折叠");
    }

    #[test]
    fn only_active_window_honors_running_rows() {
        // 会话忙时，只有最后一个 exchange 的 running 行让容器恒展开；
        // 前面历史窗口即使有残留 Running 行也不受影响。
        let rows = vec![user(1), tool(1, true), user(2), tool(2, true)];
        let ops = plan(&rows, Detail::Quiet, &Overrides::default(), true);
        let (RenderOp::Turn { spec: first, .. }, RenderOp::Turn { spec: last, .. }) =
            (&ops[0], &ops[1])
        else {
            panic!("应有两个整轮");
        };
        assert!(first.collapsible, "历史轮在会话忙时也不该被残留行锁死");
        assert!(!last.collapsible, "活跃轮的运行中工具恒展开");
        assert!(last.open);
    }

    #[test]
    fn queued_user_after_running_turn_keeps_turn_active() {
        // 回归：会话忙时用户点 Send 排队，会在跑着的轮后追加一条裸 User 行。
        // 真正在跑的过程窗口不是最后一个，但仍必须视为活跃、恒展开不可折叠。
        let rows = vec![user(1), tool(1, true), user(2)]; // user2 是排队消息，无过程行
        let ops = plan(&rows, Detail::Quiet, &Overrides::default(), true);
        let RenderOp::Turn { spec: running, .. } = &ops[0] else {
            panic!("应是整轮");
        };
        assert!(!running.collapsible, "在跑的轮不该被排队消息误判为历史轮");
        assert!(running.open);
        // 末尾那条裸 User 行单独成窗，因无过程行而退化为普通 Row。
        assert!(matches!(ops.last(), Some(RenderOp::Row(_))));
    }

    #[test]
    fn force_open_opens_every_container() {
        let rows = vec![user(1), tool(1, false), assistant(1)];
        let over = Overrides {
            force_open: true,
            ..Overrides::default()
        };
        let ops = plan(&rows, Detail::Quiet, &over, false);
        let RenderOp::Turn { spec: turn, inner } = &ops[0] else {
            panic!("应是整轮");
        };
        assert!(turn.open);
        let RenderOp::Group { spec: group, .. } = inner
            .iter()
            .find(|op| matches!(op, RenderOp::Group { .. }))
            .unwrap()
        else {
            unreachable!()
        };
        assert!(group.open);
    }
}
