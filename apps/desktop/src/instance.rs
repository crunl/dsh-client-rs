//! 单实例：同一个 profile 只允许一个 cowork 在跑。
//!
//! 双击两次会得到两个进程、两个 host，一起读写同一个 `$DSH_HOME`——没必要也危险。
//! 用 Unix socket 当锁：`bind` 是原子的，且进程没了 socket 就没人 `connect` 得上。
//! 位置在临时目录，按 profile 分开（`COWORK_PROFILE` 可以起第二个隔离实例）。

use std::io::ErrorKind;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

/// 拿到的锁；drop 时把 socket 文件删掉。
///
/// `bound` 为 `None` 表示锁没拿成（比如临时目录不可写）：app 照开，只是不去保护。
pub struct Guard {
    path: Option<PathBuf>,
    _listener: Option<UnixListener>,
}

/// 锁的结局。
pub enum Acquired {
    /// 拿到锁，可以开窗口。
    Guard(Guard),
    /// 已经有一个同名实例在跑。
    AlreadyRunning,
}

/// 按 profile 名抢锁。
pub fn acquire(name: &str) -> Acquired {
    let path = lock_path(name);
    // 先探活：连得上就是真有人在跑；连不上说明是上次崩溃留下的空壳，删掉重来。
    if UnixStream::connect(&path).is_ok() {
        return Acquired::AlreadyRunning;
    }
    let _ = std::fs::remove_file(&path);
    match UnixListener::bind(&path) {
        Ok(listener) => Acquired::Guard(Guard {
            path: Some(path),
            _listener: Some(listener),
        }),
        Err(error) if error.kind() == ErrorKind::AddrInUse => Acquired::AlreadyRunning,
        Err(error) => {
            // 锁拿不到不该拦住用户用 app：记一条，继续开（只是不防双开）。
            tracing::warn!(%error, path = %path.display(), "单实例锁不可用");
            Acquired::Guard(Guard {
                path: None,
                _listener: None,
            })
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn lock_path(name: &str) -> PathBuf {
    // profile 名进文件名前先掐掉可疑字符，避免路径注入。
    let safe: String = name
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    std::env::temp_dir().join(format!("cowork-{safe}.sock"))
}
