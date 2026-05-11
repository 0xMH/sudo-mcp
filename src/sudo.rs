use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::{SudoRunInput, REASON_ENV, ROLE_ENV};

const DEFAULT_TIMEOUT_SECONDS: u64 = 120;
const MAX_TIMEOUT_SECONDS: u64 = 3600;
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const TIMEOUT_CLEANUP_GRACE: Duration = Duration::from_secs(5);

pub async fn run_sudo(input: SudoRunInput, self_path: &str) -> Result<String> {
    if input.argv.is_empty() {
        return Err(anyhow!("argv must be a non-empty list"));
    }
    if input.reason.is_empty() {
        return Err(anyhow!(
            "reason is required so the user knows what they are authorizing"
        ));
    }

    let sudo_bin = which_executable("sudo").ok_or_else(|| anyhow!("sudo not found on PATH"))?;

    let timeout_secs = match input.timeout_seconds {
        Some(0) | None => DEFAULT_TIMEOUT_SECONDS,
        Some(n) => n.min(MAX_TIMEOUT_SECONDS),
    };
    let timeout_dur = Duration::from_secs(timeout_secs);

    let mut cmd = Command::new(&sudo_bin);
    cmd.arg("-A").arg("--").args(&input.argv);
    cmd.env("SUDO_ASKPASS", self_path);
    cmd.env(ROLE_ENV, "askpass");
    cmd.env(REASON_ENV, &input.reason);
    if let Some(dir) = &input.cwd {
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd.spawn()?;
    let pid = child.id();

    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");

    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf).await;
        buf
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        buf
    });

    let mut status: Option<std::process::ExitStatus> = None;
    let mut timed_out = false;

    tokio::select! {
        res = child.wait() => {
            status = Some(res?);
        }
        _ = tokio::time::sleep(timeout_dur) => {
            timed_out = true;
        }
    }

    if timed_out {
        #[cfg(unix)]
        if let Some(pid) = pid {
            crate::process::kill_process_group(pid as i32, &sudo_bin).await;
        }
        #[cfg(not(unix))]
        let _ = pid;

        match timeout(TIMEOUT_CLEANUP_GRACE, child.wait()).await {
            Ok(Ok(s)) => status = Some(s),
            _ => {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
        }
    }

    let stdout_bytes = timeout(TIMEOUT_CLEANUP_GRACE, stdout_task)
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();
    let stderr_bytes = timeout(TIMEOUT_CLEANUP_GRACE, stderr_task)
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();

    if timed_out {
        return Ok(format!(
            "sudo-mcp: command timed out after {}s",
            timeout_secs
        ));
    }

    let exit_code = status.and_then(|s| s.code()).unwrap_or(-1);
    Ok(format!(
        "exit_code: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        exit_code,
        truncate(&stdout_bytes),
        truncate(&stderr_bytes),
    ))
}

fn truncate(bytes: &[u8]) -> String {
    if bytes.len() <= MAX_OUTPUT_BYTES {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let head = String::from_utf8_lossy(&bytes[..MAX_OUTPUT_BYTES]).into_owned();
    let omitted = bytes.len() - MAX_OUTPUT_BYTES;
    format!("{head}\n... [truncated, {omitted} bytes omitted]")
}

fn which_executable(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&candidate) {
                if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                    return Some(candidate);
                }
            }
        }
        #[cfg(not(unix))]
        {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
