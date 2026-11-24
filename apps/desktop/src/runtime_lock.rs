//! Runtime 状态锁：基于 `flock` 的 RAII guard。
//!
//! 不变量：对 runtime 状态（state.json + runtimes/<id>）任意时刻至多一个执行者在
//! 「检查→修改→发布」临界区；锁所有权绑定活 fd，进程被 SIGKILL 时 OS 自动释放。
//! 锁文件 `runtime.lock` 只作 rendezvous 点、永不 remove——没有 PID 就没有 PID 复用，
//! 没有 remove_file 就没有释放窗口的 TOCTOU。
//!
//! 用 `flock` 而非自研 PID lockfile：后者把所有权绑在「文件存在 + PID 存活」这种
//! 所有者死后仍可被重新解释的用户态数据上，PID 复用会误判、残留文件会卡死。

use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

/// 有界等待上限：绝不无限阻塞。GUI 窗口先开、host 在专用线程，界面不冻。
const WAIT_LIMIT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// 持锁期间独占 `runtime.lock` 的 guard；Drop 释放。
#[derive(Debug)]
pub struct RuntimeLockGuard {
    file: File,
}

impl RuntimeLockGuard {
    /// 获取独占锁；已被持有时有界等待（30s），超时返回明确错误。
    pub fn acquire(path: &Path) -> Result<Self, String> {
        Self::acquire_with(path, WAIT_LIMIT, POLL_INTERVAL)
    }

    /// 可注入超时/轮询间隔的实现，供单测用短超时确定性地覆盖等待与超时分支。
    fn acquire_with(path: &Path, limit: Duration, interval: Duration) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("创建锁目录 {} 失败: {error}", parent.display()))?;
        }
        // create(true) 而非 create_new：陈旧锁文件（含死 PID）直接复用，不读其内容。
        // truncate(false)：所有权只在活 fd 的 flock 上，文件内容无关紧要，不清空。
        let mut options = OpenOptions::new();
        options.create(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // 0600：锁文件若可被同机其他用户只读打开，对方可持 LOCK_SH 长期阻塞我们的
            // LOCK_EX（DoS）。macOS flock 对只读 fd 也认 SH，所以必须在创建时就收紧。
            options.mode(0o600);
        }
        let file = options
            .open(path)
            .map_err(|error| format!("打开 runtime 锁 {} 失败: {error}", path.display()))?;
        // mode 只在新建时生效；复用旧文件时显式收紧权限，关闭同一 DoS 面。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = file
                .metadata()
                .map_err(|error| format!("读取 runtime 锁权限失败: {error}"))?
                .permissions()
                .mode();
            if mode & 0o077 != 0 {
                fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
                    format!("收紧 runtime 锁 {} 权限失败: {error}", path.display())
                })?;
            }
        }
        let deadline = Instant::now() + limit;
        loop {
            match flock_fd(&file, libc::LOCK_EX | libc::LOCK_NB) {
                Ok(()) => return Ok(Self { file }),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "等待 runtime 锁 {} 超时（{}s）：另一个操作仍在进行",
                            path.display(),
                            limit.as_secs()
                        ));
                    }
                    thread::sleep(interval);
                }
                Err(error) => {
                    return Err(format!("获取 runtime 锁 {} 失败: {error}", path.display()));
                }
            }
        }
    }
}

impl Drop for RuntimeLockGuard {
    fn drop(&mut self) {
        let _ = flock_fd(&self.file, libc::LOCK_UN);
    }
}

/// 全代码库唯一的 `unsafe`：单个 `libc::flock` 调用。
/// fd 来自本模块打开的 [`File`]，调用期间始终有效；flock 只接收 fd 与整数 flag，
/// 不读写裸指针，无生命周期逃逸。
#[allow(unsafe_code)]
fn flock_fd(file: &File, operation: libc::c_int) -> io::Result<()> {
    unsafe {
        if libc::flock(file.as_raw_fd(), operation) == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_lock_path(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("cowork-flock-{name}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir.join("runtime.lock")
    }

    #[test]
    fn guard_drop_releases_lock() {
        let path = temp_lock_path("drop");
        let first = RuntimeLockGuard::acquire(&path).unwrap();
        // 同进程第二次 open 得到独立 file description，flock 应冲突。
        let busy = RuntimeLockGuard::acquire_with(
            &path,
            Duration::from_millis(120),
            Duration::from_millis(10),
        );
        assert!(busy.is_err(), "持锁时第二个 acquire 应超时");
        assert!(busy.unwrap_err().contains("超时"));
        drop(first);
        // 释放后立即可再获取（锁文件仍在，但 flock 已解）。
        let again = RuntimeLockGuard::acquire_with(
            &path,
            Duration::from_millis(120),
            Duration::from_millis(10),
        );
        assert!(again.is_ok(), "Drop 后应能重新持锁: {:?}", again.err());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn stale_lockfile_with_dead_pid_is_ignored() {
        let path = temp_lock_path("stale");
        // 模拟旧实现留下的锁文件：写死一个不存在的 PID。
        fs::write(&path, "99999999\n").unwrap();
        let guard = RuntimeLockGuard::acquire_with(
            &path,
            Duration::from_millis(120),
            Duration::from_millis(10),
        );
        assert!(guard.is_ok(), "陈旧锁文件不应阻塞: {:?}", guard.err());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn lock_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_lock_path("mode");
        let _guard = RuntimeLockGuard::acquire(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "锁文件必须 0600，防止其他用户持 SH 锁 DoS");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn wait_times_out_and_reports_busy() {
        let path = temp_lock_path("busy");
        let _holder = RuntimeLockGuard::acquire(&path).unwrap();
        let started = Instant::now();
        let error = RuntimeLockGuard::acquire_with(
            &path,
            Duration::from_millis(80),
            Duration::from_millis(10),
        )
        .unwrap_err();
        assert!(error.contains("超时"), "应有明确超时错误: {error}");
        assert!(
            started.elapsed() >= Duration::from_millis(80),
            "应真实等待到超时"
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
