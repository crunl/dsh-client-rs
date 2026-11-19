//! host 生命周期：对齐官方 desktop——无视本机已在跑的 dsh web，
//! 永远拉起自己的 host（`dsh --profile cowork`），见 DECISIONS #41。
//!
//! 产品形态是「打开即用」：双击 `cowork.app` 不该要求用户先在终端里起 host。
//! 启动顺序：
//!
//! 1. 有 `DSH_READY_URL` 就直接 `/connect`（开发期逃生口，不是产品路径）；
//! 2. 否则向 [`runtime::RuntimeStore`] 要一个 [`runtime::ActiveRuntime`]，spawn 它的 argv；
//! 3. 自己拉起的子进程由 [`HostProcess`] 持有，app 退出时随 `Drop` 一起收掉；
//!    `DSH_READY_URL` 情形 `process` 是 `None`，退出绝不把别人的服务端带走。
//!
//! runtime 的发现、安装、选择、锁、状态与回收都在 `runtime` 模块；这里只看进程。

use crate::runtime::{ActiveRuntime, RuntimeRequest, RuntimeStore};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

/// 产品的 dsh profile 名。会话、工作区、认证都在这个 profile 下。
pub const PROFILE: &str = "cowork";

/// 新 profile 从哪个内置 profile 播种（`web` 带 HTTP/WS host API）。
const SEED_PROFILE: &str = "web";

/// 等 ready URL 的上限。冷启动要跑 cordis 装配 + 插件加载，20s 内都算正常。
const READY_TIMEOUT: Duration = Duration::from_secs(45);

/// 启动结果：连哪个 host，以及要不要替它收尸。
pub struct Startup {
    /// 带 launch token 的 ready URL，走 `/connect` 换 v2 cookie。
    pub ready_url: String,
    /// 自己拉起的 host；`DSH_READY_URL` 情形为 `None`。
    pub process: Option<HostProcess>,
    /// 握手成功后回报 ready 的 runtime id；外部 host 为 `None`。
    pub runtime_id: Option<String>,
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

/// 拉起自己的 host（开发期可用 `DSH_READY_URL` 直接连外部）；不复用别人的进程（#41）。
pub fn start(store: &RuntimeStore, request: RuntimeRequest) -> Result<Startup, String> {
    if let Ok(url) = std::env::var("DSH_READY_URL") {
        tracing::info!("使用外部 host（DSH_READY_URL）");
        return Ok(Startup {
            ready_url: url,
            process: None,
            runtime_id: None,
        });
    }

    let home = dsh_home();
    let profile = std::env::var("COWORK_PROFILE").unwrap_or_else(|_| PROFILE.to_owned());
    start_isolated(store, request, &home, &profile)
}

/// 显式 home/profile 的启动入口，给 verify 用：忽略 `DSH_READY_URL`，不碰用户状态。
pub fn start_isolated(
    store: &RuntimeStore,
    request: RuntimeRequest,
    home: &Path,
    profile: &str,
) -> Result<Startup, String> {
    let launch: ActiveRuntime = store.activate(request)?;
    let seeded = home
        .join("profiles")
        .join(profile)
        .join("package.json")
        .exists();

    // 用 `/bin/sh` 当看门狗：`[NSApp terminate:]` 会直接退进程，Rust 的 Drop 跑不到，
    // 光靠 Drop 会漏下一个几百 MB 的 node。看门狗每一秒 `kill -0` 一次父进程，
    // 父进程没了就把 node 一并收掉；它自己也是子进程，app 正常退出时由 Drop 杀。
    // desktop 工况的 V8 参数（DECISIONS #45）：legacy node 由 runtime 模块已带
    // `--max-semi-space-size=4`；新 runtime 的 launcher 自带同样设置。
    let mut host_args: Vec<String> = launch.argv().to_vec();
    host_args.push("--profile".to_owned());
    host_args.push(profile.to_owned());
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
        .env("DSH_HOME", home)
        // host 的日志里有 ANSI 与进度条，抓 URL 时用不上。
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    tracing::info!(
        runtime = %launch.display_path().display(),
        origin = %launch.origin(),
        profile,
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
        runtime_id: Some(launch.id().to_owned()),
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

#[cfg(test)]
mod tests {
    use super::redact_token;

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
}
