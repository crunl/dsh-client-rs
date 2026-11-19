//! host 生命周期：对齐官方 desktop——无视本机已在跑的 dsh web，
//! 永远拉起自己的 host（`dsh --profile cowork`），见 DECISIONS #41。
//!
//! 产品形态是「打开即用」：双击 `cowork.app` 不该要求用户先在终端里起 host。
//! 启动顺序：
//!
//! 1. 有 `DSH_READY_URL` 就直接 `/connect`（开发期逃生口，不是产品路径）；
//! 2. 否则自己 spawn 内置运行时，从输出里抓 ready URL；
//! 3. 自己拉起的子进程由 [`HostProcess`] 持有，app 退出时随 `Drop` 一起收掉；
//!    `DSH_READY_URL` 情形 `process` 是 `None`，退出绝不把别人的服务端带走。
//!
//! 状态层（会话/工作区/凭据）在 `$DSH_HOME` 级共享，运行时层（进程/profile/peer）
//! 独占：dsh web 在跑也各起各的，端口由 OS 分配互不相撞。
//!
//! 运行时位置按下面顺序找（先新架构、后兼容路径）：
//!
//! ```text
//! $COWORK_RUNTIME                             显式目录（开发/排障）
//! Resources/runtime-manifest.json             runtime package 清单
//!   ├─ Resources/<archive>                    内嵌离线包
//!   ├─ $COWORK_RUNTIME_URL                    在线下载
//!   └─ ~/.cowork/runtimes/<id>/               安装后的不可变 runtime
//! Resources/runtime                           legacy 内置完整 node_modules
//! .runtime-staging                            仓库里的开发期运行时
//! ```
//!
//! runtime package 对外只暴露 `bin/cowork-host`：它自己知道 Node 和 dsh 入口。
//! legacy 路径仍保留，避免旧版 app 和新版构建脚本互相打断。

use serde::Deserialize;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 产品的 dsh profile 名。会话、工作区、认证都在这个 profile 下。
pub const PROFILE: &str = "cowork";

/// 新 profile 从哪个内置 profile 播种（`web` 带 HTTP/WS host API）。
const SEED_PROFILE: &str = "web";

/// 等 ready URL 的上限。冷启动要跑 cordis 装配 + 插件加载，20s 内都算正常。
const READY_TIMEOUT: Duration = Duration::from_secs(45);

/// `bin.js` 相对 legacy runtime 目录的位置。
const BIN_PATH: &str = "node_modules/@deepseek-ai/dsh/lib/bin.js";

/// app bundle 内的 runtime package 清单。
const RUNTIME_MANIFEST: &str = "runtime-manifest.json";

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
}

#[derive(Debug, Deserialize)]
struct InstalledRuntime {
    schema: u32,
    id: String,
}

/// 最终启动命令的来源。新 runtime 包只暴露 launcher；legacy 仍直接拼 node 参数。
enum HostLaunch {
    Runtime { dir: PathBuf, launcher: PathBuf },
    Legacy { runtime: PathBuf, node: PathBuf },
}

impl HostLaunch {
    fn display_path(&self) -> &Path {
        match self {
            Self::Runtime { dir, .. } => dir,
            Self::Legacy { runtime, .. } => runtime,
        }
    }
}

/// 拉起后的 host 子进程。drop 即杀。
pub struct HostProcess {
    /// 看门狗（`/bin/sh`），它负责在 app 猝死时收掉 node。
    child: Child,
    /// 真正的 host：`node bin.js --profile cowork`。
    node_pid: u32,
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        // 先送 node，再看门狗（反过来的话看门狗一死，node 就没人看着了）。
        // 这足以应付 `run()` 返回的正常退出；[NSApp terminate:] 那条路不走 Drop，
        // 靠看门狗发现父进程没了之后自己收尾。
        kill_pid(self.node_pid);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 给 ``/bin/sh`` 用的单引号转义。
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// `kill -9`。失败就当它已经没了。
fn kill_pid(pid: u32) {
    let _ = Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// 启动结果：连哪个 host，以及要不要替它收尸。
pub struct Startup {
    /// 带 launch token 的 ready URL，走 `/connect` 换 v2 cookie。
    pub ready_url: String,
    /// 自己拉起的 host；`DSH_READY_URL` 情形为 `None`。
    pub process: Option<HostProcess>,
}

/// 拉起自己的 host（开发期可用 `DSH_READY_URL` 直接连外部）；不复用别人的进程（#41）。
pub fn start() -> Result<Startup, String> {
    if let Ok(url) = std::env::var("DSH_READY_URL") {
        tracing::info!("使用外部 host（DSH_READY_URL）");
        return Ok(Startup {
            ready_url: url,
            process: None,
        });
    }

    let home = dsh_home();
    let launch = resolve_host_launch()?;
    let profile = std::env::var("COWORK_PROFILE").unwrap_or_else(|_| PROFILE.to_owned());
    let seeded = home
        .join("profiles")
        .join(&profile)
        .join("package.json")
        .exists();

    // 用 `/bin/sh` 当看门狗：`[NSApp terminate:]` 会直接退进程，Rust 的 Drop 跑不到，
    // 光靠 Drop 会漏下一个几百 MB 的 node。看门狗每一秒 `kill -0` 一次父进程，
    // 父进程没了就把 node 一并收掉；它自己也是子进程，app 正常退出时由 Drop 杀。
    // 桌面工况的 V8 参数（DECISIONS #45）：node 默认按「长时间跑的服务端」调参，
    // semi-space 会 commit 到 64MB，而实测我们只用掉 1.2MB。钉到 4MB 后 idle
    // RSS 371MB → 309MB（-62MB，≈17%），冷启不变。
    //
    // 不钉 `--max-old-space-size`：能再省 ≈8MB，但重度会话（长上下文、大文件）
    // 真撞上限就是 OOM 崩宿主，不值。
    let mut host_args = match &launch {
        HostLaunch::Runtime { launcher, .. } => vec![launcher.display().to_string()],
        HostLaunch::Legacy { runtime, node } => vec![
            node.display().to_string(),
            "--max-semi-space-size=4".to_owned(),
            runtime.join(BIN_PATH).display().to_string(),
        ],
    };
    host_args.push("--profile".to_owned());
    host_args.push(profile.clone());
    if !seeded {
        host_args.push("--from-default-profile".to_owned());
        host_args.push(SEED_PROFILE.to_owned());
    }
    host_args.extend(["--no-open".to_owned(), "--port".to_owned(), "0".to_owned()]);
    let quoted = host_args
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let script = format!(
        "{quoted} & child=$!; echo \"COWORK_HOST_PID=$child\"; \
         while kill -0 $PPID 2>/dev/null; do \
             kill -0 $child 2>/dev/null || exit 0; \
             sleep 1; \
         done; kill -9 $child 2>/dev/null; wait $child 2>/dev/null"
    );

    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(script)
        .env("DSH_HOME", &home)
        // host 的日志里有 ANSI 与进度条，抓 URL 时用不上。
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    tracing::info!(
        runtime = %launch.display_path().display(),
        profile = %profile,
        seeded,
        home = %home.display(),
        "拉起 host"
    );

    let mut child = command
        .spawn()
        .map_err(|error| format!("启动 host 失败: {error}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // 两条流都可能有 URL（dsh 的 ready 行走 stdout，但被包一层时会去 stderr），
    // 也都要持续排空，否则管道写满会把 host 卡死。
    let (lines_tx, lines_rx) = channel::<String>();
    let (eof_tx, eof_rx) = channel::<()>();
    spawn_reader(stdout, lines_tx.clone(), eof_tx.clone());
    spawn_reader(stderr, lines_tx, eof_tx);

    let ready = match wait_ready(lines_rx, eof_rx) {
        Ok(ready) => ready,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };

    tracing::info!(pid = ready.node_pid, watchdog = child.id(), url = %redact_token(&ready.url), "host ready");
    Ok(Startup {
        ready_url: ready.url,
        process: Some(HostProcess {
            child,
            node_pid: ready.node_pid,
        }),
    })
}

/// 把一条流接到 channel 上，同时把后端积的日志尾巴留着（超时后报错用）。
fn spawn_reader<R: std::io::Read + Send + 'static>(
    stream: Option<R>,
    lines: std::sync::mpsc::Sender<String>,
    eof: std::sync::mpsc::Sender<()>,
) {
    let Some(stream) = stream else { return };
    std::thread::Builder::new()
        .name("cowork-host-log".to_owned())
        .spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                // 这条 channel 的接收端在抓到 URL 后就丢了，继续读只是为了排空管道。
                let _ = lines.send(line);
            }
            let _ = eof.send(());
        })
        .ok();
}

/// 在超时前从 host 的输出里找到 ready URL。
/// 等 ready 的结果：URL 与真正 host 的 pid（看门狗上报的）。
struct Ready {
    url: String,
    node_pid: u32,
}

fn wait_ready(lines: Receiver<String>, eof: Receiver<()>) -> Result<Ready, String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut tail: Vec<String> = Vec::new();
    let mut node_pid: Option<u32> = None;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!(
                "等 host 的 ready URL 超时（{}s）。最后几行：\n{}",
                READY_TIMEOUT.as_secs(),
                tail.join("\n")
            ));
        }
        match lines.recv_timeout(left) {
            Ok(line) => {
                let clean = strip_ansi(&line);
                // 看门狗第一件事就把 node 的 pid 报上来，后面超时/退出才能精确收拾它。
                if let Some(pid) = clean
                    .trim()
                    .strip_prefix("COWORK_HOST_PID=")
                    .and_then(|text| text.trim().parse::<u32>().ok())
                {
                    node_pid = Some(pid);
                    continue;
                }
                // 日志行可能带前缀，URL 出现在中间；只认带 token 的那条。
                if let Some(url) = extract_ready_url(&clean) {
                    return Ok(Ready {
                        url,
                        node_pid: node_pid.unwrap_or(0),
                    });
                }
                if !clean.trim().is_empty() {
                    // host 输出可能带含 token 的 ready 行；进日志/尾巴前统一脱敏。
                    let safe = redact_token(&clean);
                    tracing::debug!(line = %safe, "host");
                    tail.push(safe);
                    if tail.len() > 40 {
                        tail.remove(0);
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                // 两条流都断了；再看一眼 `eof` 有没有到，避免把它当成超时。
                let _ = eof.try_recv();
                return Err(format!(
                    "host 退出，没给出 ready URL。最后几行：\n{}",
                    tail.join("\n")
                ));
            }
        }
    }
}

/// 去掉 ANSI 转义（`ESC [ … m` 这类）与 `\r`。
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            continue;
        }
        if ch != '\r' {
            out.push(ch);
        }
    }
    out
}

/// 把 host 输出里 `token=…` 的值脱敏：launch 凭据不得进日志与报错文案。
/// 只替换值（到空白或 `&` 为止），行内其余部分原样保留供排障。
fn redact_token(text: &str) -> String {
    const MARK: &str = "token=";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find(MARK) {
        out.push_str(&rest[..pos + MARK.len()]);
        out.push_str("***");
        rest = &rest[pos + MARK.len()..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '&')
            .unwrap_or(rest.len());
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// 从一行里抠出 `http://127.0.0.1:PORT/?token=…`。
fn extract_ready_url(line: &str) -> Option<String> {
    let start = line.find("http://127.0.0.1:")?;
    let rest = &line[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let candidate = &rest[..end];
    // ready 行一定带 token；裸的 host 根地址连不上（会 401）。
    if !candidate.contains("?token=") {
        return None;
    }
    Some(candidate.to_owned())
}

/// `$DSH_HOME`，缺省 `~/.dsh`。
fn dsh_home() -> PathBuf {
    if let Ok(home) = std::env::var("DSH_HOME") {
        return PathBuf::from(home);
    }
    let user = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
    PathBuf::from(user).join(".dsh")
}

/// 解析最终启动方式：新 runtime package 优先，legacy 路径兜底。
fn resolve_host_launch() -> Result<HostLaunch, String> {
    if let Ok(explicit) = std::env::var("COWORK_RUNTIME") {
        let dir = PathBuf::from(explicit);
        return legacy_or_package(&dir);
    }

    if let Some(resources) = resources_dir() {
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
            let dir = ensure_runtime_installed(&manifest, &resources)?;
            let launcher = dir.join(&manifest.launcher);
            if !launcher.is_file() {
                return Err(format!("runtime 缺少 launcher：{}", launcher.display()));
            }
            return Ok(HostLaunch::Runtime { dir, launcher });
        }

        let bundled = resources.join("runtime");
        if bundled.join(BIN_PATH).is_file() {
            let node = locate_node(&bundled);
            return Ok(HostLaunch::Legacy {
                runtime: bundled,
                node,
            });
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        let staging = cwd.join(".runtime-staging");
        if staging.join(BIN_PATH).is_file() {
            let node = locate_node(&staging);
            return Ok(HostLaunch::Legacy {
                runtime: staging,
                node,
            });
        }
    }

    Err(format!(
        "找不到 dsh runtime。请设置 COWORK_RUNTIME，或在 app 中放入 {RUNTIME_MANIFEST} / legacy runtime。"
    ))
}

fn legacy_or_package(dir: &Path) -> Result<HostLaunch, String> {
    let launcher = dir.join("bin/cowork-host");
    if launcher.is_file() {
        return Ok(HostLaunch::Runtime {
            dir: dir.to_path_buf(),
            launcher,
        });
    }
    if dir.join(BIN_PATH).is_file() {
        return Ok(HostLaunch::Legacy {
            runtime: dir.to_path_buf(),
            node: locate_node(dir),
        });
    }
    Err(format!(
        "COWORK_RUNTIME 指向的目录既没有 bin/cowork-host，也没有 {BIN_PATH}：{}",
        dir.display()
    ))
}

fn resources_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
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
        || manifest.id.contains("\\")
        || manifest.id.contains("..")
    {
        return Err(format!("非法 runtime id: {}", manifest.id));
    }
    if manifest.archive.contains('/') || manifest.archive.contains("\\") {
        return Err(format!("非法 runtime archive: {}", manifest.archive));
    }
    if manifest.launcher.contains("..") || manifest.launcher.starts_with('/') {
        return Err(format!("非法 runtime launcher: {}", manifest.launcher));
    }
    Ok(manifest)
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

fn runtime_install_root() -> PathBuf {
    let home = std::env::var("COWORK_HOME")
        .unwrap_or_else(|_| std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned()));
    PathBuf::from(home).join(".cowork").join("runtimes")
}

fn ensure_runtime_installed(
    manifest: &RuntimeManifest,
    resources: &Path,
) -> Result<PathBuf, String> {
    let root = runtime_install_root();
    fs::create_dir_all(&root).map_err(|error| format!("创建 {} 失败: {error}", root.display()))?;
    let final_dir = root.join(&manifest.id);
    if installed_runtime_matches(&final_dir, manifest) {
        return Ok(final_dir);
    }
    if final_dir.exists() {
        let bad = root.join(format!("{}.bad-{}", manifest.id, unix_nanos()));
        fs::rename(&final_dir, &bad)
            .map_err(|error| format!("隔离损坏 runtime {} 失败: {error}", final_dir.display()))?;
    }

    let lock_path = root.join(format!(".{}.lock", manifest.id));
    let lock = acquire_install_lock(&lock_path)?;
    clean_stale_install_dirs(&root, &manifest.id)?;

    let result = install_runtime(manifest, resources, &root, &final_dir);
    drop(lock);
    let _ = fs::remove_file(&lock_path);
    result
}

fn open_install_lock(path: &Path) -> std::io::Result<fs::File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

fn acquire_install_lock(path: &Path) -> Result<fs::File, String> {
    let describe = |error: &std::io::Error| {
        format!(
            "无法获取 runtime 安装锁 {}（另一个安装正在进行？）: {error}",
            path.display()
        )
    };

    let mut file = match open_install_lock(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            let pid = fs::read_to_string(path)
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok());
            if let Some(pid) = pid
                && pid > 0
                && process_alive(pid)
            {
                return Err(format!(
                    "runtime 安装锁被进程 {pid} 持有：{}",
                    path.display()
                ));
            }
            let _ = fs::remove_file(path);
            open_install_lock(path).map_err(|error| describe(&error))?
        }
        Err(error) => return Err(describe(&error)),
    };

    let _ = writeln!(file, "{}", std::process::id());
    let _ = file.sync_all();
    Ok(file)
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
                    "清理未完成 runtime 安装目录 {} 失败: {error}",
                    entry.path().display()
                )
            })?;
        }
    }
    Ok(())
}

fn process_alive(pid: i32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn install_runtime(
    manifest: &RuntimeManifest,
    resources: &Path,
    root: &Path,
    final_dir: &Path,
) -> Result<PathBuf, String> {
    let embedded = resources.join(&manifest.archive);
    let is_downloaded = !embedded.is_file();
    let env_url = std::env::var("COWORK_RUNTIME_URL").ok();
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
        if !installed_runtime_matches(&temp_dir, manifest) {
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
    let Ok(text) = fs::read_to_string(dir.join("runtime.json")) else {
        return false;
    };
    let Ok(spec) = serde_json::from_str::<InstalledRuntime>(&text) else {
        return false;
    };
    spec.schema == 1 && spec.id == manifest.id && dir.join(&manifest.launcher).is_file()
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

fn unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

/// node 可执行文件。GUI 启动的 app 拿不到 shell 的 PATH，所以绝对路径优先。
fn locate_node(runtime: &Path) -> PathBuf {
    if let Ok(explicit) = std::env::var("COWORK_NODE") {
        return PathBuf::from(explicit);
    }
    // 打包时写下的绝对路径（见 cowork/build.sh）。
    for anchor in [
        runtime.join("node-path.txt"),
        runtime.join("../node-path.txt"),
    ] {
        if let Ok(text) = std::fs::read_to_string(&anchor) {
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
    // 最后退回 PATH 查找；`Command::new("node")` 会走 execvp。
    PathBuf::from("node")
}

#[cfg(test)]
mod tests {
    use super::{read_runtime_manifest, redact_token, sha256_file, target_triple};
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn redact_token_hides_the_value_but_keeps_the_shape() {
        assert_eq!(
            redact_token("web: http://127.0.0.1:50837/?token=abcDEF123"),
            "web: http://127.0.0.1:50837/?token=***"
        );
        // 值后面还有别的 query 参数或日志尾巴时，只吃掉值本身。
        assert_eq!(
            redact_token("url=http://x/?token=a&y=2 tail"),
            "url=http://x/?token=***&y=2 tail"
        );
        assert_eq!(redact_token("no credentials here"), "no credentials here");
    }

    #[test]
    fn runtime_manifest_is_parsed_and_validated() {
        let json = r#"{
            "schema": 1,
            "id": "dsh-0.2.2-rc.3+node-24.20.0+darwin-arm64",
            "target": "darwin-arm64",
            "archive": "runtime.coworkrt.tar.gz",
            "sha256": "deadbeef",
            "size": 42,
            "launcher": "bin/cowork-host",
            "url": ""
        }"#;
        let path =
            std::env::temp_dir().join(format!("cowork-manifest-{}.json", std::process::id()));
        fs::write(&path, json).unwrap();
        let manifest = read_runtime_manifest(&path).unwrap();
        assert_eq!(manifest.id, "dsh-0.2.2-rc.3+node-24.20.0+darwin-arm64");
        assert_eq!(manifest.launcher, "bin/cowork-host");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn runtime_manifest_rejects_path_traversal() {
        let json = r#"{
            "schema": 1,
            "id": "../escape",
            "target": "darwin-arm64",
            "archive": "runtime.tar.gz",
            "sha256": "deadbeef",
            "size": 42,
            "launcher": "bin/cowork-host"
        }"#;
        let path =
            std::env::temp_dir().join(format!("cowork-bad-manifest-{}.json", std::process::id()));
        fs::write(&path, json).unwrap();
        assert!(read_runtime_manifest(&path).is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn sha256_file_matches_known_digest() {
        let path: PathBuf =
            std::env::temp_dir().join(format!("cowork-sha256-{}.txt", std::process::id()));
        fs::write(&path, "hello\n").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn target_triple_is_not_empty() {
        assert!(!target_triple().is_empty());
    }
}
