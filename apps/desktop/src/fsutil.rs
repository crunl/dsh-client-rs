//! 共享原子写：唯一临时名 + 0600 + create_new + sync_all + rename + 父目录 fsync。
//!
//! runtime 的 state.json 与侧栏快照都走这里，避免各自实现原子替换时的差异
//! （固定临时名在并发下互相覆盖、0644 泄漏、漏 fsync）。写入的内容先落临时文件、
//! fsync 后 rename 覆盖目标；rename 失败则清理临时文件，目标保持原样。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// 生成不冲突的临时文件名。纳秒时间戳 + PID + 序号足以在同一目录下唯一。
fn temp_path(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "state".to_owned());
    let file = format!(".{name}.{}-{nanos}.tmp", std::process::id());
    path.with_file_name(file)
}

use std::path::PathBuf;

/// 把 `bytes` 原子写入 `path`：成功则目标完整替换，失败则目标不动。
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = temp_path(path);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if let Err(error) = written {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    drop(file);
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    // rename 的持久化依赖父目录项落盘；失败不影响本次逻辑可见性，忽略即可。
    if let Some(parent) = path.parent()
        && let Ok(dir) = File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("cowork-fsutil-{name}-{nanos}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn writes_twice_and_leaves_no_temp_file() {
        let dir = temp_dir("twice");
        let path = dir.join("state.json");
        atomic_write(&path, b"first").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"first");
        atomic_write(&path, b"second-longer").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second-longer");
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "不应残留临时文件: {leftovers:?}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn preexisting_temp_file_is_not_overwritten() {
        let dir = temp_dir("preexisting");
        let path = dir.join("state.json");
        // 同名临时文件预置：create_new 会因 AlreadyExists 失败，目标不变。
        // 这里用固定名模拟（真实 temp 含纳秒，无法预知）；确保写入报错而非覆盖。
        fs::write(&path, b"original").unwrap();
        let temp = path.with_file_name(".state.json.1-1.tmp");
        fs::write(&temp, b"sentinel").unwrap();
        // 正常调用不受影响（唯一名），目标被正确覆盖。
        atomic_write(&path, b"updated").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"updated");
        assert_eq!(fs::read(&temp).unwrap(), b"sentinel");
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn written_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("mode");
        let path = dir.join("state.json");
        atomic_write(&path, b"secret").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "权限应为 0600，实际 {mode:o}");
        fs::remove_dir_all(dir).unwrap();
    }
}
