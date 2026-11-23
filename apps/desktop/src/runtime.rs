//! Runtime 生命周期：发现、安装、选择、校验、回收。
//!
//! 这是 runtime 的唯一外部 seam。`host` 只拿 [`ActiveRuntime::argv`] 去 spawn，
//! 不再知道 manifest、tar、锁、状态文件或 legacy node 查找。
//!
//! 用户状态与 runtime 状态都在 `$DSH_HOME`（缺省 `~/.dsh`）；runtime 落在
//! `$DSH_HOME/profiles/<profile>/runtimes/`，与 dsh profile 共根，不再用 `~/.cowork`。

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const RUNTIME_MANIFEST: &str = "runtime-manifest.json";
const BIN_PATH: &str = "node_modules/@deepseek-ai/dsh/lib/bin.js";
const STATE_FILE: &str = "state.json";
const RUNTIME_LOCK: &str = "runtime.lock";
const QUARANTINE_DIR: &str = "runtime-quarantine";

/// 环境变量读取器。生产传 `env::var`，测试传闭包 pin 值——这样路径解析可纯函数测试，
/// 不必改进程全局 env（Rust 2024 下 `set_var` 是 unsafe 且多线程读写即 UB）。
type EnvGetter = dyn Fn(&str) -> Option<String>;

/// 解析 dsh home，对齐官方 `@deepseek-ai/dsh-home-paths` 的 `resolveDshHome()`：
/// `$DSH_HOME`（空白视为未设置）优先，否则 `~/.dsh`；再展开 `~`/`~/` 前缀、
/// 以 cwd 为基准 `resolve` 成归一化绝对路径。Rust 侧与注入给 dsh 的 DSH_HOME
/// 必须一致，否则 runtime 与 profile 数据分裂、播种重试守卫失效。
fn resolve_dsh_home_with(get: &EnvGetter, cwd: &Path) -> PathBuf {
    let from_env = get("DSH_HOME").filter(|v| !v.trim().is_empty());
    let selected = from_env
        .map(PathBuf::from)
        .unwrap_or_else(|| os_home_with(get).join(".dsh"));
    normalize_absolute(&expand_home(&selected, &os_home_with(get)), cwd)
}

/// 跨平台用户主目录，对应 Node `os.homedir()`。
fn os_home_with(get: &EnvGetter) -> PathBuf {
    // Node os.homedir(): Windows 用 USERPROFILE（缺失时 HOMEDRIVE+HOMEPATH），
    // POSIX 用 HOME（缺失时应查 passwd，这里沿用 /tmp 回退）。
    if cfg!(windows) {
        if let Some(profile) = get("USERPROFILE").filter(|v| !v.is_empty()) {
            return PathBuf::from(profile);
        }
        if let (Some(drive), Some(path)) = (get("HOMEDRIVE"), get("HOMEPATH")) {
            return PathBuf::from(format!("{drive}{path}"));
        }
    }
    if let Some(home) = get("HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(home);
    }
    PathBuf::from("/tmp")
}

/// 展开 `~` / `~/` / `~\` 前缀，其余原样返回（对齐官方 `expandHomePath`）。
fn expand_home(path: &Path, home: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
        return home.join(rest);
    }
    path.to_path_buf()
}

/// 相对路径以 cwd 为基准转绝对，并词法归一化 `.` / `..`（对齐 Node `path.resolve`）。
/// 只做词法处理、不要求路径存在，等价于官方 `resolve` 而不触碰文件系统。
fn normalize_absolute(path: &Path, cwd: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        use std::path::Component;
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 全进程唯一的 home 解析点；`host.rs` 与 verify 都复用它。
pub fn dsh_home() -> PathBuf {
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    resolve_dsh_home_with(&|key| env::var(key).ok(), &cwd)
}

/// 产品 profile 名，与 `host::start` 传给 dsh 的 `--profile` 同源（都读 `COWORK_PROFILE`，
/// 缺省 `cowork`）。runtime 目录必须跟着它走，否则覆盖 profile 时 runtime 与 dsh 数据错位。
fn profile_name() -> String {
    env::var("COWORK_PROFILE").unwrap_or_else(|_| "cowork".to_owned())
}

/// runtime 状态根目录：`<dsh_home>/profiles/<profile>`。其下 `runtimes/`、`state.json`
/// 等都归本模块管理。dsh 自己把 profile 配置也放这里，但只在播种时整体重建 profile
/// 目录的内容——runtime 的幂等重装（见 `ensure_installed`）保证它被删后能自愈。
fn runtime_home_from(dsh_home: &Path, profile: &str) -> PathBuf {
    dsh_home.join("profiles").join(profile)
}

pub fn runtime_home() -> PathBuf {
    runtime_home_from(&dsh_home(), &profile_name())
}

/// 选择哪个 runtime。调用者只需要给一个封闭请求，不需要知道目录规则。
#[derive(Debug, Clone)]
pub enum RuntimeRequest {
    /// app manifest 的正常路径，内部再按 env / pin 决策。
    Default,
    /// 已安装 runtime id；回滚或 CLI 显式选择。
    Id(String),
    /// 开发期直接给目录。
    Dir(PathBuf),
}

/// 持久化的默认选择。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Selection {
    #[default]
    Auto,
    Pin {
        id: String,
    },
}

/// 唯一的状态写入口。
#[derive(Debug, Clone)]
pub enum RuntimeControl {
    Use(Selection),
    MarkReady(String),
    MarkVerified { id: String, suite: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectMode {
    DryRun,
    Apply,
}

/// host 启动所需的最小信息。
#[derive(Debug, Clone)]
pub struct ActiveRuntime {
    id: String,
    origin: String,
    dir: PathBuf,
    /// 可执行文件和固定前置参数；`--profile` 等由 host 追加。
    argv: Vec<String>,
}

impl ActiveRuntime {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn display_path(&self) -> &Path {
        &self.dir
    }

    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    fn view(&self) -> ActiveRuntimeView {
        ActiveRuntimeView {
            id: self.id.clone(),
            origin: self.origin.clone(),
            dir: self.dir.clone(),
            argv: self.argv.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ActiveRuntimeView {
    pub id: String,
    pub origin: String,
    pub dir: PathBuf,
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verification {
    pub id: String,
    pub suite: String,
    pub at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstalledRuntimeView {
    pub id: String,
    pub dir: PathBuf,
    pub launcher: PathBuf,
    pub profile: Option<String>,
    pub dsh: Option<String>,
    pub node: Option<String>,
    pub size_bytes: u64,
    pub valid: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeStatus {
    pub desired: Option<String>,
    pub desired_profile: Option<String>,
    pub selection: Selection,
    pub selected: Option<String>,
    pub last_good: Option<String>,
    pub last_verified: Option<Verification>,
    pub installed: Vec<InstalledRuntimeView>,
    pub protected: Vec<String>,
    pub gc_candidates: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CollectReport {
    pub mode: &'static str,
    pub protected: Vec<String>,
    pub candidates: Vec<String>,
    pub moved: Vec<String>,
    pub quarantine: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct RuntimeState {
    schema: u32,
    selection: Selection,
    last_good: Option<String>,
    last_verified: Option<Verification>,
    history: Vec<HistoryEvent>,
    bootstrap_protected: Vec<String>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            schema: 1,
            selection: Selection::Auto,
            last_good: None,
            last_verified: None,
            history: Vec::new(),
            bootstrap_protected: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistoryEvent {
    at: String,
    kind: String,
    id: Option<String>,
    detail: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RuntimeManifest {
    schema: u32,
    id: String,
    target: String,
    archive: String,
    sha256: String,
    size: u64,
    launcher: String,
    url: Option<String>,
    profile: Option<String>,
    #[allow(dead_code)]
    exclusions: Option<BTreeMap<String, u64>>,
}

#[derive(Debug, Clone, Deserialize)]
struct InstalledRuntime {
    schema: u32,
    id: String,
    launcher: Option<String>,
    profile: Option<String>,
    dsh: Option<String>,
    node: Option<String>,
}

#[derive(Debug, Clone)]
struct InstalledInfo {
    id: String,
    dir: PathBuf,
    view: InstalledRuntimeView,
}

#[derive(Debug, Clone)]
pub struct RuntimeStore {
    resources: Option<PathBuf>,
    install_root: PathBuf,
    state_path: PathBuf,
    lock_path: PathBuf,
    quarantine_root: PathBuf,
}

impl RuntimeStore {
    pub fn from_env() -> Self {
        Self::at(runtime_home())
    }

    /// 用显式的 runtime 状态根目录构造，消除第二份路径拼接。
    fn at(base: PathBuf) -> Self {
        Self::with_resources(base, resources_dir())
    }

    /// `at` 的可注入版本；测试传 `None` 隔离真实 app bundle。
    fn with_resources(base: PathBuf, resources: Option<PathBuf>) -> Self {
        Self {
            resources,
            install_root: base.join("runtimes"),
            state_path: base.join(STATE_FILE),
            lock_path: base.join(RUNTIME_LOCK),
            quarantine_root: base.join(QUARANTINE_DIR),
        }
    }

    pub fn activate(&self, request: RuntimeRequest) -> Result<ActiveRuntime, String> {
        match request {
            RuntimeRequest::Dir(dir) => legacy_or_package(&dir),
            RuntimeRequest::Id(id) => self.by_id(&id),
            RuntimeRequest::Default => self.activate_default(),
        }
    }

    fn activate_default(&self) -> Result<ActiveRuntime, String> {
        if let Ok(dir) = env::var("COWORK_RUNTIME") {
            return legacy_or_package(&PathBuf::from(dir));
        }
        if let Ok(id) = env::var("COWORK_RUNTIME_ID") {
            return self.by_id(&id);
        }
        let state = self.load_state()?;
        if let Selection::Pin { id } = &state.selection {
            return self.by_id(id);
        }
        self.resolve_default()
    }

    fn by_id(&self, id: &str) -> Result<ActiveRuntime, String> {
        validate_runtime_id(id)?;
        let dir = self.install_root.join(id);
        let launcher = installed_launcher(&dir, id)?;
        if !launcher.is_file() {
            return Err(format!(
                "runtime {id} 缺少 launcher：{}",
                launcher.display()
            ));
        }
        Ok(ActiveRuntime {
            id: id.to_owned(),
            origin: "pinned".to_owned(),
            dir,
            argv: vec![launcher.display().to_string()],
        })
    }

    fn resolve_default(&self) -> Result<ActiveRuntime, String> {
        if let Some(resources) = &self.resources {
            let manifest_path = resources.join(RUNTIME_MANIFEST);
            if manifest_path.is_file() {
                let manifest = read_runtime_manifest(&manifest_path)?;
                if manifest.target != target_triple() {
                    return Err(format!(
                        "runtime package 目标不匹配：需要 {}，清单是 {}",
                        target_triple(),
                        manifest.target
                    ));
                }
                let dir = self.ensure_installed(&manifest, resources)?;
                let launcher = installed_launcher(&dir, &manifest.id)?;
                if !launcher.is_file() {
                    return Err(format!("runtime 缺少 launcher：{}", launcher.display()));
                }
                return Ok(ActiveRuntime {
                    id: manifest.id,
                    origin: "manifest".to_owned(),
                    dir,
                    argv: vec![launcher.display().to_string()],
                });
            }

            let bundled = resources.join("runtime");
            if bundled.join(BIN_PATH).is_file() {
                return Ok(legacy_runtime(bundled));
            }
        }

        if let Ok(cwd) = env::current_dir() {
            let staging = cwd.join(".runtime-staging");
            if staging.join(BIN_PATH).is_file() {
                return Ok(legacy_runtime(staging));
            }
        }

        Err(format!(
            "找不到 dsh runtime。请设置 COWORK_RUNTIME，或在 app 中放入 {RUNTIME_MANIFEST} / legacy runtime。"
        ))
    }

    pub fn status(&self) -> Result<RuntimeStatus, String> {
        let state = self.load_state()?;
        let installed = self.scan_installed();
        let desired = self.desired();
        let desired_profile = desired.as_ref().and_then(|(_, profile)| profile.clone());
        let desired_id = desired.map(|(id, _)| id);
        let selected = match self.selection_override(&state) {
            Some(id) => Some(id),
            None => desired_id.clone(),
        };
        let protected = self.protected_ids(&state, &desired_id);
        let candidates = installed
            .iter()
            .filter(|item| item.view.valid && !protected.contains(&item.id))
            .map(|item| item.id.clone())
            .collect();

        Ok(RuntimeStatus {
            desired: desired_id,
            desired_profile,
            selection: state.selection,
            selected,
            last_good: state.last_good,
            last_verified: state.last_verified,
            installed: installed.into_iter().map(|item| item.view).collect(),
            protected: protected.into_iter().collect(),
            gc_candidates: candidates,
        })
    }

    pub fn control(&self, command: RuntimeControl) -> Result<(), String> {
        self.with_state_lock(|| {
            let mut state = self.load_state()?;
            let now = now_string();
            match command {
                RuntimeControl::Use(Selection::Auto) => {
                    state.selection = Selection::Auto;
                    // 显式选择意味着用户接手版本管理；迁移期的一揽子保护到此结束。
                    state.bootstrap_protected.clear();
                    push_history(
                        &mut state,
                        HistoryEvent {
                            at: now,
                            kind: "use-auto".to_owned(),
                            id: None,
                            detail: None,
                        },
                    );
                }
                RuntimeControl::Use(Selection::Pin { id }) => {
                    validate_runtime_id(&id)?;
                    installed_launcher(&self.install_root.join(&id), &id)?;
                    state.selection = Selection::Pin { id: id.clone() };
                    // 显式选择意味着用户接手版本管理；迁移期的一揽子保护到此结束。
                    state.bootstrap_protected.clear();
                    push_history(
                        &mut state,
                        HistoryEvent {
                            at: now,
                            kind: "use-pin".to_owned(),
                            id: Some(id),
                            detail: None,
                        },
                    );
                }
                RuntimeControl::MarkReady(id) => {
                    if id.is_empty() || id == "legacy" {
                        return Ok(());
                    }
                    state.last_good = Some(id.clone());
                    push_history(
                        &mut state,
                        HistoryEvent {
                            at: now,
                            kind: "ready".to_owned(),
                            id: Some(id),
                            detail: None,
                        },
                    );
                }
                RuntimeControl::MarkVerified { id, suite } => {
                    validate_runtime_id(&id)?;
                    installed_launcher(&self.install_root.join(&id), &id)?;
                    state.last_verified = Some(Verification {
                        id: id.clone(),
                        suite,
                        at: now.clone(),
                    });
                    push_history(
                        &mut state,
                        HistoryEvent {
                            at: now,
                            kind: "verified".to_owned(),
                            id: Some(id),
                            detail: None,
                        },
                    );
                }
            }
            self.write_state(&state)
        })
    }

    pub fn collect(&self, mode: CollectMode) -> Result<CollectReport, String> {
        self.with_state_lock(|| {
            let mut state = self.load_state()?;
            let desired = self.desired_id();
            let protected = self.protected_ids(&state, &desired);
            let installed = self.scan_installed();
            let candidates: Vec<String> = installed
                .iter()
                .filter(|item| item.view.valid && !protected.contains(&item.id))
                .map(|item| item.id.clone())
                .collect();

            let mut moved = Vec::new();
            if mode == CollectMode::Apply && !candidates.is_empty() {
                fs::create_dir_all(&self.quarantine_root).map_err(|error| {
                    format!("创建 {} 失败: {error}", self.quarantine_root.display())
                })?;
                let ids: BTreeSet<String> = candidates.iter().cloned().collect();
                for item in installed {
                    if !ids.contains(&item.id) {
                        continue;
                    }
                    let target = self
                        .quarantine_root
                        .join(format!("{}-{}", item.id, unix_nanos()));
                    fs::rename(&item.dir, &target).map_err(|error| {
                        format!(
                            "隔离 runtime {} 到 {} 失败: {error}",
                            item.dir.display(),
                            target.display()
                        )
                    })?;
                    moved.push(item.id);
                }
                state.bootstrap_protected.retain(|id| !moved.contains(id));
                push_history(
                    &mut state,
                    HistoryEvent {
                        at: now_string(),
                        kind: "gc".to_owned(),
                        id: None,
                        detail: Some(moved.join(",")),
                    },
                );
                self.write_state(&state)?;
            }

            Ok(CollectReport {
                mode: match mode {
                    CollectMode::DryRun => "dry-run",
                    CollectMode::Apply => "applied",
                },
                protected: protected.into_iter().collect(),
                candidates,
                moved,
                quarantine: self.quarantine_root.clone(),
            })
        })
    }

    fn selection_override(&self, state: &RuntimeState) -> Option<String> {
        if let Ok(id) = env::var("COWORK_RUNTIME_ID") {
            return Some(id);
        }
        match &state.selection {
            Selection::Auto => None,
            Selection::Pin { id } => Some(id.clone()),
        }
    }

    fn protected_ids(&self, state: &RuntimeState, desired: &Option<String>) -> BTreeSet<String> {
        let mut ids = BTreeSet::new();
        if let Some(id) = desired {
            ids.insert(id.clone());
        }
        if let Ok(id) = env::var("COWORK_RUNTIME_ID") {
            ids.insert(id);
        }
        if let Selection::Pin { id } = &state.selection {
            ids.insert(id.clone());
        }
        if let Some(id) = &state.last_good {
            ids.insert(id.clone());
        }
        if let Some(verified) = &state.last_verified {
            ids.insert(verified.id.clone());
        }
        ids.extend(state.bootstrap_protected.iter().cloned());
        ids
    }

    fn desired(&self) -> Option<(String, Option<String>)> {
        let resources = self.resources.as_ref()?;
        let path = resources.join(RUNTIME_MANIFEST);
        if path.is_file() {
            return read_runtime_manifest(&path)
                .ok()
                .map(|manifest| (manifest.id, manifest.profile));
        }
        if resources.join("runtime").join(BIN_PATH).is_file() {
            return Some(("legacy".to_owned(), None));
        }
        None
    }

    fn desired_id(&self) -> Option<String> {
        self.desired().map(|(id, _)| id)
    }

    fn load_state(&self) -> Result<RuntimeState, String> {
        if !self.state_path.is_file() {
            let state = RuntimeState {
                bootstrap_protected: self
                    .scan_installed()
                    .into_iter()
                    .filter(|item| item.view.valid)
                    .map(|item| item.id)
                    .collect(),
                ..Default::default()
            };
            self.write_state(&state)?;
            return Ok(state);
        }
        let text = fs::read_to_string(&self.state_path)
            .map_err(|error| format!("读取 {} 失败: {error}", self.state_path.display()))?;
        let state: RuntimeState = serde_json::from_str(&text)
            .map_err(|error| format!("解析 {} 失败: {error}", self.state_path.display()))?;
        if state.schema != 1 {
            return Err(format!("不支持的 runtime state schema: {}", state.schema));
        }
        Ok(state)
    }

    fn write_state(&self, state: &RuntimeState) -> Result<(), String> {
        if let Some(parent) = self.state_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("创建 {} 失败: {error}", parent.display()))?;
        }
        let temp = self
            .state_path
            .with_extension(format!("{}.tmp", unix_nanos()));
        let text = serde_json::to_string_pretty(state)
            .map_err(|error| format!("序列化 runtime state 失败: {error}"))?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&temp)
            .map_err(|error| format!("创建 {} 失败: {error}", temp.display()))?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("写入 {} 失败: {error}", temp.display()))?;
        drop(file);
        if let Err(error) = fs::rename(&temp, &self.state_path) {
            let _ = fs::remove_file(&temp);
            return Err(format!("替换 {} 失败: {error}", self.state_path.display()));
        }
        Ok(())
    }

    fn with_state_lock<T>(&self, action: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        if let Some(parent) = self.lock_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("创建 {} 失败: {error}", parent.display()))?;
        }
        let guard = acquire_pid_lock(&self.lock_path)?;
        let result = action();
        drop(guard);
        let _ = fs::remove_file(&self.lock_path);
        result
    }

    fn scan_installed(&self) -> Vec<InstalledInfo> {
        let Ok(entries) = fs::read_dir(&self.install_root) else {
            return Vec::new();
        };
        let mut items = Vec::new();
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            // 安装中间目录和隔离目录不是可选 runtime。
            if id.starts_with('.') || id.contains(".bad-") || id.ends_with(".install") {
                continue;
            }
            let spec = read_installed_runtime(&dir, &id).ok();
            let launcher = spec
                .as_ref()
                .map(|spec| {
                    dir.join(
                        spec.launcher
                            .clone()
                            .unwrap_or_else(|| "bin/cowork-host".to_owned()),
                    )
                })
                .unwrap_or_else(|| dir.join("bin/cowork-host"));
            let valid = spec.is_some() && launcher.is_file();
            let view = InstalledRuntimeView {
                id: id.clone(),
                dir: dir.clone(),
                launcher: launcher.clone(),
                profile: spec.as_ref().and_then(|spec| spec.profile.clone()),
                dsh: spec.as_ref().and_then(|spec| spec.dsh.clone()),
                node: spec.as_ref().and_then(|spec| spec.node.clone()),
                size_bytes: dir_size(&dir),
                valid,
            };
            items.push(InstalledInfo { id, dir, view });
        }
        items.sort_by(|left, right| left.id.cmp(&right.id));
        items
    }

    fn ensure_installed(
        &self,
        manifest: &RuntimeManifest,
        resources: &Path,
    ) -> Result<PathBuf, String> {
        fs::create_dir_all(&self.install_root)
            .map_err(|error| format!("创建 {} 失败: {error}", self.install_root.display()))?;
        let final_dir = self.install_root.join(&manifest.id);
        if installed_runtime_matches(&final_dir, manifest) {
            return Ok(final_dir);
        }
        if final_dir.exists() {
            let bad = self
                .install_root
                .join(format!("{}.bad-{}", manifest.id, unix_nanos()));
            fs::rename(&final_dir, &bad).map_err(|error| {
                format!("隔离损坏 runtime {} 失败: {error}", final_dir.display())
            })?;
        }

        let lock_path = self
            .install_root
            .join(format!(".{}.install.lock", manifest.id));
        let lock = acquire_pid_lock(&lock_path)?;
        clean_stale_install_dirs(&self.install_root, &manifest.id)?;
        let result = install_runtime(manifest, resources, &self.install_root, &final_dir);
        drop(lock);
        let _ = fs::remove_file(&lock_path);
        result
    }
}

/// CLI adapter。正常 app 启动不经过这里。
pub fn cli(args: &[String]) -> Result<String, String> {
    match args.first().map(String::as_str) {
        None | Some("help") | Some("--help") => Ok(
            "用法：cowork runtime <status|activate <id>|activate --dir <path>|use <id|auto>|verify <id> <suite>|gc [--apply]>".to_owned(),
        ),
        Some("status") if args.len() == 1 => cli_status(),
        Some("activate") => cli_activate(&args[1..]),
        Some("use") if args.len() == 2 => {
            let selection = match args[1].as_str() {
                "auto" => Selection::Auto,
                id => Selection::Pin { id: id.to_owned() },
            };
            RuntimeStore::from_env().control(RuntimeControl::Use(selection))?;
            Ok("ok\n".to_owned())
        }
        Some("verify") => cli_verify(&args[1..]),
        Some("gc") => cli_gc(&args[1..]),
        Some(other) => Err(format!("未知 runtime 子命令：{other}")),
    }
}

fn cli_status() -> Result<String, String> {
    let status = RuntimeStore::from_env().status()?;
    serde_json::to_string_pretty(&status).map_err(|error| format!("序列化状态失败: {error}"))
}

fn cli_activate(args: &[String]) -> Result<String, String> {
    let request = match args {
        [id] => RuntimeRequest::Id(id.clone()),
        [flag, dir] if flag == "--dir" => RuntimeRequest::Dir(PathBuf::from(dir)),
        _ => return Err("用法：cowork runtime activate <id>|--dir <path>".to_owned()),
    };
    let active = RuntimeStore::from_env().activate(request)?;
    serde_json::to_string_pretty(&active.view())
        .map_err(|error| format!("序列化 activate 结果失败: {error}"))
}

fn cli_verify(args: &[String]) -> Result<String, String> {
    let [id, suite] = args else {
        return Err("用法：cowork runtime verify <id> <suite>".to_owned());
    };
    let store = RuntimeStore::from_env();
    let active = store.activate(RuntimeRequest::Id(id.clone()))?;
    let output = Command::new(&active.argv()[0])
        .args(&active.argv()[1..])
        .arg("--version")
        .output()
        .map_err(|error| format!("运行 runtime 版本探测失败: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "runtime 版本探测失败: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();

    let script = smoke_script_path()?;
    let workspace_root = workspace_root(&script)?;
    let work = temp_workspace(id)?;
    let target_home = work.join("dsh");
    fs::create_dir_all(&target_home)
        .map_err(|error| format!("创建 {} 失败: {error}", target_home.display()))?;
    let source_home = dsh_home();
    if !source_home.is_dir() {
        let _ = fs::remove_dir_all(&work);
        return Err(format!(
            "找不到源 DSH_HOME：{}（verify 需要已有会话/凭据）",
            source_home.display()
        ));
    }
    // verify 只需要源 home 的会话/凭据；runtime 安装树（node+dsh，可达数 GB）与隔离区
    // 由真实 install_root 提供，拷进临时目录纯属浪费——迁移后两者共根，必须显式跳过。
    let runtime_subtree = Path::new("profiles").join("cowork");
    let runtimes_rel = runtime_subtree.join("runtimes");
    let quarantine_rel = runtime_subtree.join(QUARANTINE_DIR);
    copy_tree_contents(&source_home, &target_home, &|rel| {
        rel == runtimes_rel || rel == quarantine_rel
    })?;

    let outcome = (|| -> Result<(), String> {
        let startup = crate::host::start_isolated(
            &store,
            RuntimeRequest::Id(id.clone()),
            &target_home,
            "cowork",
        )?;
        let url_file = write_ready_url(&work, &startup.ready_url)?;
        let status = Command::new("bash")
            .arg(&script)
            .arg(&url_file)
            .current_dir(&workspace_root)
            .env("RUNTIME_SMOKE_ISOLATED", "1")
            .status()
            .map_err(|error| format!("运行 {} 失败: {error}", script.display()))?;
        // 先杀 host 再删临时 DSH_HOME。
        drop(startup);
        if !status.success() {
            return Err(format!("smoke 脚本失败: {status}"));
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&work);
    outcome?;

    store.control(RuntimeControl::MarkVerified {
        id: id.clone(),
        suite: suite.clone(),
    })?;
    Ok(format!(
        "version={version}\nverified={id}\nsuite={suite}\nsmoke=ok\n"
    ))
}

fn smoke_script_path() -> Result<PathBuf, String> {
    if let Ok(path) = env::var("COWORK_SMOKE_SCRIPT") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path
                .canonicalize()
                .map_err(|error| format!("解析 {} 失败: {error}", path.display()));
        }
        return Err(format!(
            "COWORK_SMOKE_SCRIPT 指向的文件不存在：{}",
            path.display()
        ));
    }
    let cwd = env::current_dir().map_err(|error| format!("读取当前目录失败: {error}"))?;
    for ancestor in cwd.ancestors() {
        let candidate = ancestor.join("scripts/runtime/smoke-runtime.sh");
        if candidate.is_file() {
            return candidate
                .canonicalize()
                .map_err(|error| format!("解析 {} 失败: {error}", candidate.display()));
        }
    }
    Err(
        "找不到 scripts/runtime/smoke-runtime.sh；请在仓库根目录运行，或设置 COWORK_SMOKE_SCRIPT"
            .to_owned(),
    )
}

fn workspace_root(script: &Path) -> Result<PathBuf, String> {
    script
        .ancestors()
        .find(|dir| dir.join("Cargo.toml").is_file())
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("找不到 {} 所属的 Cargo workspace", script.display()))
}

fn temp_workspace(id: &str) -> Result<PathBuf, String> {
    let safe_id: String = id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '+' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let path = env::temp_dir().join(format!("cowork-verify-{safe_id}-{}", unix_nanos()));
    fs::create_dir(&path).map_err(|error| format!("创建 {} 失败: {error}", path.display()))?;
    #[cfg(unix)]
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("设置 {} 权限失败: {error}", path.display()))?;
    Ok(path)
}

fn write_ready_url(work: &Path, url: &str) -> Result<PathBuf, String> {
    let path = work.join("ready-url");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&path)
        .map_err(|error| format!("创建 {} 失败: {error}", path.display()))?;
    file.write_all(url.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("写入 {} 失败: {error}", path.display()))?;
    Ok(path)
}

/// 递归复制 src 的内容到 dst。`skip` 收到相对 src 的路径，返回 true 则跳过该项。
fn copy_tree_contents(src: &Path, dst: &Path, skip: &dyn Fn(&Path) -> bool) -> Result<(), String> {
    let entries =
        fs::read_dir(src).map_err(|error| format!("读取 {} 失败: {error}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("读取 {} 项失败: {error}", src.display()))?;
        let source = entry.path();
        let name = entry.file_name();
        let rel = Path::new(&name);
        if skip(rel) {
            continue;
        }
        let target = dst.join(rel);
        let meta = fs::symlink_metadata(&source)
            .map_err(|error| format!("读取 {} 元数据失败: {error}", source.display()))?;
        let kind = meta.file_type();
        if kind.is_dir() {
            fs::create_dir_all(&target)
                .map_err(|error| format!("创建 {} 失败: {error}", target.display()))?;
            copy_tree_contents(&source, &target, &|child| skip(&rel.join(child)))?;
        } else if kind.is_file() {
            fs::copy(&source, &target).map_err(|error| {
                format!(
                    "复制 {} 到 {} 失败: {error}",
                    source.display(),
                    target.display()
                )
            })?;
        } else if kind.is_symlink() {
            let link = fs::read_link(&source)
                .map_err(|error| format!("读取符号链接 {} 失败: {error}", source.display()))?;
            symlink(link, &target)
                .map_err(|error| format!("创建符号链接 {} 失败: {error}", target.display()))?;
        }
    }
    Ok(())
}

fn cli_gc(args: &[String]) -> Result<String, String> {
    let mode = match args {
        [] => CollectMode::DryRun,
        [flag] if flag == "--apply" => CollectMode::Apply,
        _ => return Err("用法：cowork runtime gc [--apply]".to_owned()),
    };
    let report = RuntimeStore::from_env().collect(mode)?;
    serde_json::to_string_pretty(&report).map_err(|error| format!("序列化回收报告失败: {error}"))
}

fn legacy_runtime(dir: PathBuf) -> ActiveRuntime {
    let node = locate_node(&dir);
    let entry = dir.join(BIN_PATH);
    ActiveRuntime {
        id: "legacy".to_owned(),
        origin: "legacy".to_owned(),
        dir,
        argv: vec![
            node.display().to_string(),
            "--max-semi-space-size=4".to_owned(),
            entry.display().to_string(),
        ],
    }
}

fn legacy_or_package(dir: &Path) -> Result<ActiveRuntime, String> {
    let launcher = dir.join("bin/cowork-host");
    if launcher.is_file() {
        let id = read_installed_runtime(dir, "runtime")
            .map(|spec| spec.id)
            .unwrap_or_else(|_| "path".to_owned());
        return Ok(ActiveRuntime {
            id,
            origin: "path".to_owned(),
            dir: dir.to_path_buf(),
            argv: vec![launcher.display().to_string()],
        });
    }
    if dir.join(BIN_PATH).is_file() {
        return Ok(legacy_runtime(dir.to_path_buf()));
    }
    Err(format!(
        "COWORK_RUNTIME 指向的目录既没有 bin/cowork-host，也没有 {BIN_PATH}：{}",
        dir.display()
    ))
}

fn resources_dir() -> Option<PathBuf> {
    let exe = env::current_exe().ok()?;
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    Some(contents.join("Resources"))
}

fn read_runtime_manifest(path: &Path) -> Result<RuntimeManifest, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("读取 {} 失败: {error}", path.display()))?;
    let manifest: RuntimeManifest = serde_json::from_str(&text)
        .map_err(|error| format!("解析 {} 失败: {error}", path.display()))?;
    if manifest.schema != 1 {
        return Err(format!(
            "不支持的 runtime manifest schema: {}",
            manifest.schema
        ));
    }
    if manifest.id.is_empty()
        || manifest.id.contains('/')
        || manifest.id.contains('\\')
        || manifest.id.contains("..")
    {
        return Err(format!("非法 runtime id: {}", manifest.id));
    }
    if manifest.archive.contains('/') || manifest.archive.contains('\\') {
        return Err(format!("非法 runtime archive: {}", manifest.archive));
    }
    if manifest.launcher.contains("..") || manifest.launcher.starts_with('/') {
        return Err(format!("非法 runtime launcher: {}", manifest.launcher));
    }
    Ok(manifest)
}

fn read_installed_runtime(dir: &Path, expected_id: &str) -> Result<InstalledRuntime, String> {
    let text = fs::read_to_string(dir.join("runtime.json"))
        .map_err(|error| format!("读取 {} 失败: {error}", dir.join("runtime.json").display()))?;
    let spec: InstalledRuntime = serde_json::from_str(&text)
        .map_err(|error| format!("解析 {} 失败: {error}", dir.join("runtime.json").display()))?;
    if spec.schema != 1 {
        return Err(format!("不支持的 runtime.json schema: {}", spec.schema));
    }
    if expected_id != "runtime" && spec.id != expected_id {
        return Err(format!(
            "runtime id 不匹配：目录 {}，清单 {}",
            expected_id, spec.id
        ));
    }
    if let Some(launcher) = &spec.launcher
        && (launcher.contains("..") || launcher.starts_with('/'))
    {
        return Err(format!("非法 runtime launcher: {launcher}"));
    }
    Ok(spec)
}

fn installed_launcher(dir: &Path, id: &str) -> Result<PathBuf, String> {
    validate_runtime_id(id)?;
    let spec = read_installed_runtime(dir, id)?;
    let launcher = dir.join(
        spec.launcher
            .unwrap_or_else(|| "bin/cowork-host".to_owned()),
    );
    if !launcher.is_file() {
        return Err(format!(
            "runtime {id} 缺少 launcher：{}",
            launcher.display()
        ));
    }
    Ok(launcher)
}

fn validate_runtime_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.contains('/') || id.contains('\\') || id.contains("..") {
        return Err(format!("非法 runtime id: {id}"));
    }
    Ok(())
}

fn target_triple() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "darwin-arm64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "darwin-x64"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "linux-x64"
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )))]
    {
        "unsupported"
    }
}

fn acquire_pid_lock(path: &Path) -> Result<fs::File, String> {
    let describe = |error: &std::io::Error| {
        format!(
            "无法获取 runtime 锁 {}（另一个操作正在进行？）: {error}",
            path.display()
        )
    };
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            let pid = fs::read_to_string(path)
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok());
            if let Some(pid) = pid
                && pid > 0
                && process_alive(pid)
            {
                return Err(format!("runtime 锁被进程 {pid} 持有：{}", path.display()));
            }
            let _ = fs::remove_file(path);
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|error| describe(&error))?
        }
        Err(error) => return Err(describe(&error)),
    };
    let _ = writeln!(file, "{}", std::process::id());
    let _ = file.sync_all();
    Ok(file)
}

fn process_alive(pid: i32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn clean_stale_install_dirs(root: &Path, id: &str) -> Result<(), String> {
    let prefix = format!(".{id}-");
    let entries = fs::read_dir(root).map_err(|error| format!("读取 runtime 目录失败: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("读取 runtime 目录项失败: {error}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix) && name.ends_with(".install") && entry.path().is_dir() {
            fs::remove_dir_all(entry.path()).map_err(|error| {
                format!(
                    "清理未完成 runtime 目录 {} 失败: {error}",
                    entry.path().display()
                )
            })?;
        }
    }
    Ok(())
}

fn install_runtime(
    manifest: &RuntimeManifest,
    resources: &Path,
    root: &Path,
    final_dir: &Path,
) -> Result<PathBuf, String> {
    let embedded = resources.join(&manifest.archive);
    let is_downloaded = !embedded.is_file();
    let env_url = env::var("COWORK_RUNTIME_URL").ok();
    let download_url = env_url
        .as_deref()
        .filter(|url| !url.is_empty())
        .or_else(|| manifest.url.as_deref().filter(|url| !url.is_empty()));
    let archive = if !is_downloaded {
        embedded
    } else if let Some(url) = download_url {
        let downloaded = root.join(format!(".{}-{}.download", manifest.id, unix_nanos()));
        download_runtime(url, &downloaded)?;
        downloaded
    } else {
        return Err(format!(
            "runtime 包缺失，且清单没有下载 URL：{}",
            embedded.display()
        ));
    };

    let verified = (|| -> Result<(), String> {
        let size = fs::metadata(&archive)
            .map_err(|error| format!("读取 runtime 包大小失败: {error}"))?
            .len();
        if size != manifest.size {
            return Err(format!(
                "runtime 包大小不匹配：期望 {}，实际 {size}",
                manifest.size
            ));
        }
        let digest = sha256_file(&archive)?;
        if !digest.eq_ignore_ascii_case(&manifest.sha256) {
            return Err(format!(
                "runtime 包 SHA-256 不匹配：期望 {}，实际 {digest}",
                manifest.sha256
            ));
        }
        Ok(())
    })();
    if let Err(error) = verified {
        if is_downloaded {
            let _ = fs::remove_file(&archive);
        }
        return Err(error);
    }

    let temp_dir = root.join(format!(".{}-{}.install", manifest.id, unix_nanos()));
    let installed = (|| -> Result<(), String> {
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir)
            .map_err(|error| format!("创建临时 runtime 目录失败: {error}"))?;
        let status = Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&temp_dir)
            .status()
            .map_err(|error| format!("解压 runtime 包失败: {error}"))?;
        if !status.success() {
            return Err(format!("tar 解压 runtime 包失败: {status}"));
        }
        if read_installed_runtime(&temp_dir, &manifest.id).is_err()
            || !temp_dir.join(&manifest.launcher).is_file()
        {
            return Err("runtime 包内容缺失或 runtime.json 不匹配".to_owned());
        }
        fs::rename(&temp_dir, final_dir)
            .map_err(|error| format!("安装 runtime 到 {} 失败: {error}", final_dir.display()))
    })();
    let _ = fs::remove_dir_all(&temp_dir);
    if is_downloaded {
        let _ = fs::remove_file(&archive);
    }
    installed.map(|()| final_dir.to_path_buf())
}

fn download_runtime(url: &str, dest: &Path) -> Result<(), String> {
    let status = Command::new("curl")
        .arg("-fL")
        .arg("--retry")
        .arg("2")
        .arg("-o")
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|error| format!("下载 runtime 失败: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        let _ = fs::remove_file(dest);
        Err(format!("curl 下载 runtime 失败: {status}"))
    }
}

fn installed_runtime_matches(dir: &Path, manifest: &RuntimeManifest) -> bool {
    let Ok(spec) = read_installed_runtime(dir, &manifest.id) else {
        return false;
    };
    let launcher = spec
        .launcher
        .unwrap_or_else(|| "bin/cowork-host".to_owned());
    spec.schema == 1 && spec.id == manifest.id && dir.join(launcher).is_file()
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let output = Command::new("shasum")
        .arg("-a")
        .arg("256")
        .arg(path)
        .output()
        .map_err(|error| format!("计算 SHA-256 失败: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "shasum 失败: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .next()
        .map(ToOwned::to_owned)
        .ok_or_else(|| "shasum 输出为空".to_owned())
}

fn locate_node(runtime: &Path) -> PathBuf {
    if let Ok(explicit) = env::var("COWORK_NODE") {
        return PathBuf::from(explicit);
    }
    for anchor in [
        runtime.join("node-path.txt"),
        runtime.join("../node-path.txt"),
    ] {
        if let Ok(text) = fs::read_to_string(&anchor) {
            let path = PathBuf::from(text.trim());
            if path.is_file() {
                return path;
            }
        }
    }
    for fixed in [
        "/opt/homebrew/bin/node",
        "/usr/local/bin/node",
        "/opt/homebrew/opt/node/bin/node",
        "/usr/bin/node",
    ] {
        let path = PathBuf::from(fixed);
        if path.is_file() {
            return path;
        }
    }
    PathBuf::from("node")
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_dir() {
                stack.push(path);
            } else {
                total += meta.len();
            }
        }
    }
    total
}

fn push_history(state: &mut RuntimeState, event: HistoryEvent) {
    state.history.push(event);
    if state.history.len() > 50 {
        state.history.remove(0);
    }
}

fn now_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| format!("{}", duration.as_secs()))
        .unwrap_or_else(|_| "0".to_owned())
}

fn unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        CollectMode, RuntimeControl, RuntimeRequest, RuntimeStore, Selection,
        read_runtime_manifest, resolve_dsh_home_with, runtime_home_from, sha256_file,
        target_triple,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 用注入的 env getter + cwd 解析 dsh home，不碰进程全局 env（避免 Rust 2024 下
    /// `set_var` 的 unsafe 与多线程 UB）。getter 返回 `Some(value)` 表示变量存在。
    fn pinned_getter(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let owned: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn dsh_home_pin_resolves_runtime_state_under_dsh_profile() {
        let cwd = Path::new("/volatile/workdir");
        let pinned = "/volatile/dxg/.dsh";
        let vars = [("DSH_HOME", pinned)];
        let home = resolve_dsh_home_with(&pinned_getter(&vars), cwd);
        assert_eq!(home, Path::new(pinned));
        let base = runtime_home_from(&home, "cowork");
        assert_eq!(base, Path::new(pinned).join("profiles").join("cowork"));
        let store = RuntimeStore::at(base.clone());
        assert_eq!(store.install_root, base.join("runtimes"));
        assert_eq!(store.state_path, base.join("state.json"));
        assert_eq!(store.lock_path, base.join("runtime.lock"));
        assert_eq!(store.quarantine_root, base.join("runtime-quarantine"));
    }

    #[test]
    fn dsh_home_falls_back_to_os_home_and_default_profile() {
        let cwd = Path::new("/volatile/workdir");
        let fake_home = "/volatile/fake-home";
        let vars = [("HOME", fake_home)];
        let home = resolve_dsh_home_with(&pinned_getter(&vars), cwd);
        assert_eq!(home, Path::new(fake_home).join(".dsh"));
        assert_eq!(
            runtime_home_from(&home, "cowork"),
            Path::new(fake_home)
                .join(".dsh")
                .join("profiles")
                .join("cowork")
        );
    }

    #[test]
    fn dsh_home_expands_tilde_and_normalizes_relative_path() {
        let cwd = Path::new("/volatile/workdir");
        let home_dir = "/volatile/fake-home";
        // DSH_HOME=~/x 展开为 <home>/x。
        let tilde = [("DSH_HOME", "~/x"), ("HOME", home_dir)];
        assert_eq!(
            resolve_dsh_home_with(&pinned_getter(&tilde), cwd),
            Path::new(home_dir).join("x")
        );
        // 相对路径以 cwd 为基准归一化。
        let relative = [("DSH_HOME", "./a/../b")];
        assert_eq!(
            resolve_dsh_home_with(&pinned_getter(&relative), cwd),
            cwd.join("b")
        );
        // 空白 DSH_HOME 视为未设置，回退 ~/.dsh。
        let blank = [("DSH_HOME", "   "), ("HOME", home_dir)];
        assert_eq!(
            resolve_dsh_home_with(&pinned_getter(&blank), cwd),
            Path::new(home_dir).join(".dsh")
        );
    }

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("cowork-runtime-{name}-{nanos}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn store_at(root: &Path) -> RuntimeStore {
        let base = root.join("home").join("profiles").join("cowork");
        RuntimeStore::with_resources(base, None)
    }

    fn install_fake(store: &RuntimeStore, id: &str, profile: Option<&str>) {
        let dir = store.install_root.join(id);
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(dir.join("bin/cowork-host"), "#!/bin/sh\n").unwrap();
        let mut json = format!("{{\"schema\":1,\"id\":\"{id}\",\"launcher\":\"bin/cowork-host\"");
        if let Some(profile) = profile {
            json.push_str(&format!(",\"profile\":\"{profile}\""));
        }
        json.push('}');
        fs::write(dir.join("runtime.json"), json).unwrap();
    }

    #[test]
    fn state_bootstraps_existing_runtimes_as_protected() {
        let root = temp_dir("bootstrap");
        let store = store_at(&root);
        install_fake(&store, "old-runtime", None);
        let status = store.status().unwrap();
        assert!(status.protected.iter().any(|id| id == "old-runtime"));
        assert!(status.gc_candidates.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pin_rejects_missing_runtime_and_accepts_installed() {
        let root = temp_dir("pin");
        let store = store_at(&root);
        let missing = store.control(RuntimeControl::Use(Selection::Pin {
            id: "missing".to_owned(),
        }));
        assert!(missing.is_err());

        install_fake(&store, "known", None);
        store
            .control(RuntimeControl::Use(Selection::Pin {
                id: "known".to_owned(),
            }))
            .unwrap();
        let status = store.status().unwrap();
        assert_eq!(
            status.selection,
            Selection::Pin {
                id: "known".to_owned()
            }
        );
        assert_eq!(status.selected.as_deref(), Some("known"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_id_activation_uses_installed_launcher() {
        let root = temp_dir("activate");
        let store = store_at(&root);
        install_fake(&store, "pinned-runtime", Some("code"));
        let active = store
            .activate(RuntimeRequest::Id("pinned-runtime".to_owned()))
            .unwrap();
        assert_eq!(active.id(), "pinned-runtime");
        assert_eq!(active.origin(), "pinned");
        assert_eq!(active.argv().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn collect_dry_run_does_not_move_anything() {
        let root = temp_dir("gc-dry");
        let store = store_at(&root);
        install_fake(&store, "old-runtime", None);
        let report = store.collect(CollectMode::DryRun).unwrap();
        assert_eq!(report.mode, "dry-run");
        assert!(report.candidates.is_empty());
        assert!(store.install_root.join("old-runtime").is_dir());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn collect_moves_unprotected_runtime_to_quarantine() {
        let root = temp_dir("gc-apply");
        let store = store_at(&root);
        install_fake(&store, "old-runtime", None);
        // 先建立 state，bootstrap 会保护 old-runtime；清掉这一项模拟升级后确认可回收。
        let _ = store.status().unwrap();
        let mut state: super::RuntimeState =
            serde_json::from_str(&fs::read_to_string(&store.state_path).unwrap()).unwrap();
        state.bootstrap_protected.clear();
        fs::write(
            &store.state_path,
            serde_json::to_string_pretty(&state).unwrap(),
        )
        .unwrap();

        let report = store.collect(CollectMode::Apply).unwrap();
        assert_eq!(report.mode, "applied");
        assert_eq!(report.candidates, vec!["old-runtime".to_owned()]);
        assert_eq!(report.moved, vec!["old-runtime".to_owned()]);
        assert!(!store.install_root.join("old-runtime").exists());
        assert!(fs::read_dir(&store.quarantine_root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("old-runtime-")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn use_clears_bootstrap_protection() {
        let root = temp_dir("use-clears-bootstrap");
        let store = store_at(&root);
        install_fake(&store, "keep", None);
        install_fake(&store, "old", None);
        let status = store.status().unwrap();
        assert_eq!(status.gc_candidates.len(), 0);

        store
            .control(RuntimeControl::Use(Selection::Pin {
                id: "keep".to_owned(),
            }))
            .unwrap();
        let status = store.status().unwrap();
        assert!(status.protected.contains(&"keep".to_owned()));
        assert!(status.gc_candidates.contains(&"old".to_owned()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_is_parsed_and_path_traversal_rejected() {
        let root = temp_dir("manifest");
        let path = root.join("runtime-manifest.json");
        fs::write(
            &path,
            r#"{
                "schema": 1,
                "id": "dsh-0.2.2-rc.5+node-24.20.0+darwin-arm64+code",
                "target": "darwin-arm64",
                "archive": "runtime.coworkrt.tar.gz",
                "sha256": "deadbeef",
                "size": 42,
                "launcher": "bin/cowork-host",
                "profile": "code"
            }"#,
        )
        .unwrap();
        let manifest = read_runtime_manifest(&path).unwrap();
        assert_eq!(manifest.profile.as_deref(), Some("code"));

        fs::write(
            &path,
            r#"{
                "schema": 1,
                "id": "../escape",
                "target": "darwin-arm64",
                "archive": "runtime.tar.gz",
                "sha256": "deadbeef",
                "size": 42,
                "launcher": "bin/cowork-host"
            }"#,
        )
        .unwrap();
        assert!(read_runtime_manifest(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sha256_matches_known_digest() {
        let root = temp_dir("sha256");
        let path = root.join("hello.txt");
        fs::write(&path, "hello\n").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
        assert!(!target_triple().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mark_ready_and_verified_are_recorded() {
        let root = temp_dir("marks");
        let store = store_at(&root);
        install_fake(&store, "known", None);
        store
            .control(RuntimeControl::MarkReady("known".to_owned()))
            .unwrap();
        store
            .control(RuntimeControl::MarkVerified {
                id: "known".to_owned(),
                suite: "smoke-v1".to_owned(),
            })
            .unwrap();
        let status = store.status().unwrap();
        assert_eq!(status.last_good.as_deref(), Some("known"));
        assert_eq!(status.last_verified.unwrap().suite, "smoke-v1");
        fs::remove_dir_all(root).unwrap();
    }
}
