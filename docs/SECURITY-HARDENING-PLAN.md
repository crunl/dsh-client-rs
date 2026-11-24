# 安全加固实施计划（4 路第一性原则审计收敛）

> 状态：待确认实施（2026-11-23）
> 来源：OCR 全库审查 + 2 路安全审计 subagent + 4 路第一性原则方案设计 subagent。
> 所有代码事实均已逐项核实（含 Cargo.lock 版本、缓存、配置行号）。

## 0. 背景与范围

OCR 审查（排除 `chat.rs`/`markdown.rs`）产出 3 条性能 finding，已修复并通过验证（`fmt`/`clippy -D warnings`/`test` 163 passed）：
1. `connection.rs`：`from_utf8_lossy().into_owned()` 二次分配 → happy path 用 `String::from_utf8`。
2. `connection.rs`：`read_body` 用 `content_length()` fail-fast + `with_capacity` 预分配。
3. `mux.rs`：握手失败记忆 fail-fast（修复 N×超时串行握手）；审计发现其 10s 窗口误伤轮询后，已收敛为 1s 窗口 + 成功后 `clear_handshake_failure()`。

随后的安全审计发现一批既有问题（非本次 OCR 改动引入）。本计划处理其中全部 **4 个 medium**，并顺带收敛几个有共同根因的 low。

**无 critical / high 阻断项。**

## 1. 四个 Medium 的统一方案

### M1 · PATH 劫持 → 两层收敛（信任根 + 边界）

**第一性原则**：完整性校验的信任根必须建立在**进程内、直接作用于待执行字节的代码**上，不经任何经可变命名空间（PATH）解析的外部程序。经 PATH spawn 的程序本身在被 spawn 瞬间就是 RCE，所以"只修 shasum"会制造虚假安全感。

- **层 1（信任根，收敛点根因修复）**：原生 SHA-256 替换 `shasum`。`sha2 = "0.11"` 已在 Cargo.lock 且本地缓存命中（0.11.1），零新增编译单元、零网络。重写 `sha256_file`（签名不变，调用点与现有测试零改动）：分块读文件 → `Sha256::update` → 小写 hex。
- **层 2（边界，统一解析器）**：新增深模块 `apps/desktop/src/trusted_command.rs`：`trusted_command(program)` 只在 `["/usr/bin","/bin","/usr/sbin","/sbin"]` 白名单目录解析（macOS SIP 保护 root:wheel），绝不查 PATH；保留继承的 PATH 给子进程自身需要（smoke 脚本要跑 `cargo`）。`cfg(not(unix))` 退化到裸名保编译（Windows 上 `shasum` 不存在，层 1 的原生 sha2 恰好消除该破绽）。

调用点改造：`bash`/`kill`×2/`tar`/`curl`/`open` 全部改走 `trusted_command`；`shasum` 整段删除；`locate_node` 的裸名 `"node"` 回退纳入（改为报错，不静默 exec PATH 里的 node）。`/bin/sh`（已是绝对路径）、`active.argv()[0]`（来自已校验安装树）不动。

**否决**：逐点硬编码绝对路径（7 处症状补丁、破 Windows、信任根仍信外部进程）、校验 owner/group（需 FFI 违反 `unsafe_code=deny`、SIP 已给同等保证）、内建 tar（无 crate、自研解压是新攻击面）。

### M2 · 锁域不一致 + PID 复用卡死 → 一把 flock 收敛到四个 seam

**第一性原则**：不变量是"对 runtime 状态（state.json + runtimes/<id>）任意时刻至多一个执行者在'检查→修改→发布'临界区，且锁所有权与执行者生命周期绑定（执行者死，锁必死）"。根因是把所有权绑在"文件存在 + PID 存活"这种可在所有者死后继续存在、可被重新解释的用户态数据上。

**方案**：用 `libc::flock` RAII guard 替换自研 PID lockfile（`libc 0.2.191` 已在 Cargo.lock，零新增编译；`std::fs::File::try_lock_exclusive` 不存在，实测 E0599）。锁文件 `base/runtime.lock` 只作 rendezvous 点、**永不 remove**；所有权 = 活 fd 上的 flock，强退由 OS 关 fd 自动释放，没有 PID 就没有 PID 复用，没有 remove_file 就没有释放窗口 TOCTOU。

把锁从散落的 per-operation 锁收敛到 `RuntimeStore` 的**四个公开 seam**：`activate`/`status`/`control`/`collect` 各持同一把锁；所有 `*_inner` 与 `ensure_installed`/`install_runtime`/`load_state`/`write_state` 全部无锁（已逐条核对无嵌套 acquire，不会自锁）。一并删除 `acquire_pid_lock`、`process_alive`、`with_state_lock`、per-id `.install.lock`。

提取纯决策 `plan_install(final_dir, manifest, now_ns) -> InstallPlan {Current, Replace{bad}, Fresh}`（最易错的"检查匹配→隔离旧"分支单测化）。`install_runtime` 的 temp→final `fs::rename` 保留为**锁内提交点**。

- `unsafe_code = "deny"`，只在单个 5 行 FFI 封装上开 `#[allow(unsafe_code)]`（唯一 `libc::flock` 调用，无裸指针）。
- 等待策略：非阻塞 try + 有界等待（30s）+ 明确中文错误，绝不无限阻塞；GUI 窗口先开、host 在专用线程，界面不冻。
- `instance.rs` 的单实例 socket 是"发现已运行并唤起窗口"，语义不同，**不动**。
- Windows：`runtime.rs:14` 本就无条件 `use std::os::unix::fs`，当前已无法在 Windows 编译；flock 放 `#[cfg(unix)]`，不恶化现状。锁层可移植，未来换 `fs2` 不动业务代码。

### M3 · 播种守卫只覆盖 Err → ready 屏障处验证后置条件

**第一性原则**：真正的契约是"`start()` 返回 Ok ⇒ runtime 在磁盘上可用"，而非"播种发生了吗"（后者是代理，与"runtime 被删"相关而非充要）。原生缺口：macOS 上删除运行中进程的可执行文件不杀进程，host 照样 ready、`Ok` 分支从不校验；且 `MarkReady` 不校验 launcher，会把已删 runtime 静默记为 phantom `last_good`。

**方案**：ready URL 是外部破坏结束的同步屏障（播种在 `prepareProfile` 完成，ready 要等 cordis 装配 + 插件加载后，`host.rs:27-28`）。在 `host.rs` 的 `wait_ready` 返回 Ok 之后、构造 `Startup` 之前，调用新增的 `RuntimeStore::ensure_active(&launch)`：直接检查 `launcher` 是否还在，缺失且匹配清单则幂等重装（`ensure_installed`）。返回 `true` 表示确实重装。纯决策函数 `should_reinstall_after_ready(launcher_present, manifest_matches)` 可穷举单测。

**与 M2 的契合**：`ensure_active` 放在 `activate` seam 的锁内临界区尾部，与 flock 方案天然合并——"检查→重装修复"也在同一把锁下串行，无新竞态。

`should_retry_after_seed`（Err 分支，无 ready 屏障）及其单测**原样保留**。不推翻共根决策、不改 `Connection`、不做 symlink 方案（否决理由：改落点要推翻 DECISIONS #61/#55 且丢 profile 同源；symlink 被删同样断子进程、仍需检测）。

## 2. 顺带收敛的 Low（共同根因）

- **单一 profile 权威**（RC-A）：`profile_name()` 提为收敛点，加字符集净化（对齐 `instance.rs`：仅 ascii alphanumeric/`-`/`_`，掐掉其余，空→`cowork`），导出给 `host.rs:93`、`main.rs:90` 复用。单测注入式 `sanitize_profile(raw)` 纯函数。
- **共享原子写**（RC-B + fsync）：新增 `fsutil::atomic_write`（唯一临时名 + 0600 + create_new + sync_all + rename + 父目录 fsync），`runtime.rs::write_state` 与 `cache.rs::save` 都改走它。净删代码，且修复 cache 0644 + 固定临时名 + 无 sync。
- **统一瞬态 GC**（RC-C）：`gc_transient(root, id)` 覆盖三类 `.bad-*` / `.<id>-*.download` / `.<id>-*.install`，每类保留最新 1 份（无时钟依赖、单测确定）。崩溃中途的 `.download` 与从未清理的 `.bad-*` 由此回收。`quarantine_root` 不动（用户可见历史，语义不同）。
- **bootstrap 写盘进锁内**（属 M2 范畴）：`load_state()` 变纯读（缺失时内存返回 initial_state，含 bootstrap 扫描结果，**不写盘**）；在已持锁的 `control`/`collect` 中，`load_state()` 后若 state 文件不存在则先 `write_state` 再改。保留 bootstrap 扫描，否则首次 control 会用空保护集误 GC 旧 runtime。
- **不改**：`redact_token` 大小写（唯一秘密 ready URL 由 `extract_ready_url` 的小写 `?token=` 解析，格式变了连接先失败，显性故障，YAGNI）；owner/group 校验；fsync 独立立项（已被共享原子写免费带上）。

## 3. 改动文件清单

| 文件 | 改动 |
|---|---|
| `Cargo.toml` / `apps/desktop/Cargo.toml` | 加 `sha2 = "0.11"`、`libc = "0.2"`（均已在 lock） |
| `apps/desktop/src/trusted_command.rs` | 新增：可信命令解析器 |
| `apps/desktop/src/runtime_lock.rs` | 新增：flock RAII guard + 有界等待（或并入 runtime.rs） |
| `apps/desktop/src/fsutil.rs` | 新增：共享原子写 |
| `apps/desktop/src/runtime.rs` | 原生 sha256、trusted_command 调用点、flock seam 化、plan_install、ensure_active、profile_name 权威、gc_transient、load_state 纯读、调用 fsutil |
| `apps/desktop/src/host.rs` | wait_ready 后 ensure_active、profile_name 复用、trusted_command("kill") |
| `apps/desktop/src/main.rs` | profile_name 复用、trusted_command("open") |
| `apps/desktop/src/cache.rs` | save() 改走 fsutil::atomic_write |
| `docs/DECISIONS.md` | 新增 #62：本轮加固决策；补正 #61 的 MarkReady 记述 |

## 4. 验证方案（headless）

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`（确认唯一 `#[allow(unsafe_code)]` 生效、无其他违规）
- `cargo test --workspace`（现有 163 passed 须保持 + 新增单测）

新增单测：
- 层 1：现有 `sha256_matches_known_digest`（`"hello\n"` → `5891…be03`）零改动通过 + >64KB 分块测试。
- 层 2：`trusted_command` 正向（绝对路径在白名单）、负向（不存在的命令 Err）、**中毒 PATH 回归**（注入假 tar 到 PATH，断言假命令未执行）。
- flock：`guard_drop_releases`、`stale_lockfile_with_dead_pid_is_ignored`（写死 PID 仍能获取，证明 B 消除）、`wait_times_out_and_reports_busy`（注入假 clock/try_lock）、`plan_install_three_states`。
- 播种：`should_reinstall_after_ready` 四组合 + 模拟删除重装的端到端（临时目录）。
- profile：`sanitize_profile` 注入式纯函数。
- 原子写：写两次、无 tmp 残留、预置同名 tmp 未被覆盖、Unix mode 0600。
- 瞬态 GC：造三类残留，断言各只剩最新/全清、合法目录完好。

**无法 headless 验证（如实记录）**：真实 GUI 首次播种时序、macOS 删除运行中可执行文件后 host 仍 ready、真多进程 flock 竞争——需在真机补验。

## 5. 风险与取舍

- **改动面较大**（9 个文件，含 1 处 unsafe 例外），但都是既有 medium 的根因修复，非本次引入。
- **flock 的 `#[allow(unsafe_code)]`** 是唯一例外，单点 5 行、无裸指针；若团队坚持零 unsafe，可改用 `fs2`/`fd-lock`（已声明依赖原则下需新增，但 Windows 可移植更好）——需用户拍板。
- **原生 sha2 用 0.11**：本地缓存命中、零网络；若 CI 环境无缓存需能联网 fetch（0.11.1 是公开 registry 包，常规可行）。
- **所有改动可分批实施**：建议顺序 M1 → M2（含 low 的 lock/bootstrap 项）→ M3 → low 收敛，每批独立验证。
