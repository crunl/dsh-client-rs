//! 外部命令的可信解析：绝不经过进程 PATH。
//!
//! 安全敏感命令（完整性校验、解压、下载、进程信号）若经 PATH 解析，攻击者控制的
//! PATH 目录里的同名程序会在被 spawn 的瞬间以 app 权限执行任意代码；伪造的 `shasum`
//! 更会让 SHA-256 校验在数学上通过、语义上被架空。这里把程序名解析到固定系统目录
//! （macOS 由 SIP 保护、root:wheel 所有），把「查 PATH」变成「查白名单」。
//!
//! 子进程继承的 PATH 保持不变——smoke 脚本自身需要 `cargo` 等工具，它们不是本 app
//! 直接 exec 的可信命令，不在本模块的担保范围内。

use std::path::PathBuf;
use std::process::Command;

/// 由 OS 保护的固定目录。macOS 上这些目录受 SIP 保护，普通用户不可写。
#[cfg(unix)]
const TRUSTED_DIRS: &[&str] = &["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// 把程序名解析为受信任目录下的绝对路径。
///
/// 返回一个可直接 `.arg()` / `.output()` 的 [`Command`]；找不到则返回错误，不回退到
/// PATH（回退会重新打开劫持面）。
#[cfg(unix)]
pub fn trusted_command(program: &str) -> Result<Command, String> {
    // 拒绝含路径分隔符的名字，避免 "usr/bin/x" 之类绕过白名单拼接。
    if program.contains('/') || program.contains('\\') {
        return Err(format!("不可信的命令名（含路径分隔符）: {program}"));
    }
    let path = TRUSTED_DIRS
        .iter()
        .map(PathBuf::from)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| format!("受信任目录中找不到命令: {program}"))?;
    Ok(Command::new(path))
}

/// 非 Unix：本 app 当前不支持（runtime.rs 依赖 `std::os::unix`），这里只保证能编译。
/// 未来支持 Windows 时应改为 System32 白名单，届时业务调用点无需改动。
#[cfg(not(unix))]
pub fn trusted_command(program: &str) -> Result<Command, String> {
    Ok(Command::new(program))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn resolves_to_absolute_path_in_allowlist() {
        for program in ["tar", "bash", "curl", "kill", "open"] {
            let cmd = trusted_command(program)
                .unwrap_or_else(|error| panic!("{program} 应在白名单中: {error}"));
            let resolved = cmd.get_program().to_string_lossy().into_owned();
            assert!(
                TRUSTED_DIRS
                    .iter()
                    .any(|dir| resolved.starts_with(&format!("{dir}/"))),
                "{program} 解析到了白名单之外: {resolved}"
            );
            assert!(!resolved.contains(".."), "路径应已归一化: {resolved}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_missing_or_path_like_names() {
        assert!(
            trusted_command("nosuchcmd_xyz_42").is_err(),
            "不存在的命令应报错"
        );
        assert!(
            trusted_command("usr/bin/tar").is_err(),
            "含路径分隔符应拒绝"
        );
        assert!(trusted_command("../bin/sh").is_err(), "相对路径应拒绝");
    }

    /// 关键回归：即使子进程的 PATH 被污染成含假 `tar` 的目录，我们直接 exec 的仍是
    /// 白名单里的真 tar——假命令不会被 spawn，marker 文件不会被创建。
    #[cfg(unix)]
    #[test]
    fn poisoning_path_does_not_hijack_the_command() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let fake_dir = std::env::temp_dir().join(format!("cowork-trusted-cmd-{unique}"));
        std::fs::create_dir_all(&fake_dir).expect("create fake dir");
        let marker = fake_dir.join("HIJACKED");
        let fake_tar = fake_dir.join("tar");
        std::fs::write(
            &fake_tar,
            format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display()),
        )
        .expect("write fake tar");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_tar, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake tar");
        }

        let status = {
            let mut cmd = trusted_command("tar").expect("trusted tar");
            cmd.env("PATH", &fake_dir)
                .arg("--version")
                .output()
                .expect("run trusted tar")
        };

        assert!(!marker.exists(), "假 tar 被执行了——PATH 劫持未被切断");
        assert!(status.status.success(), "真 tar --version 应成功");
        assert!(
            !String::from_utf8_lossy(&status.stdout).contains("HIJACKED"),
            "输出来自假命令"
        );

        let _ = std::fs::remove_dir_all(&fake_dir);
    }
}
