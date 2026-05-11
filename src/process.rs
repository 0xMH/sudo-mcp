use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use tokio::process::Command;
use tokio::time::timeout;

const PRIVILEGED_KILL_MAX_WAIT: Duration = Duration::from_secs(2);

pub async fn kill_process_group(pgid: i32, sudo_bin: &Path) {
    // Root-owned children may reject the user's signal. When sudo has a fresh
    // timestamp and sudoers allows it, this kills them without another prompt.
    try_privileged_kill(pgid, sudo_bin).await;
    let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
}

async fn try_privileged_kill(pgid: i32, sudo_bin: &Path) {
    if sudo_bin.as_os_str().is_empty() {
        return;
    }
    let Some(kill_bin) = system_kill_path() else {
        return;
    };

    let mut cmd = Command::new(sudo_bin);
    cmd.arg("-n")
        .arg("--")
        .arg(&kill_bin)
        .arg("-KILL")
        .arg(format!("-{pgid}"));
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());

    if let Ok(mut child) = cmd.spawn() {
        if timeout(PRIVILEGED_KILL_MAX_WAIT, child.wait())
            .await
            .is_err()
        {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

fn system_kill_path() -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    for path in ["/bin/kill", "/usr/bin/kill"] {
        let p = PathBuf::from(path);
        if let Ok(meta) = std::fs::metadata(&p) {
            if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use tokio::process::Command;
    use tokio::time::sleep;

    #[tokio::test]
    async fn kills_process_group_children() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ready = dir.path().join("ready");
        let survived = dir.path().join("survived");

        let script = format!(
            "(trap '' HUP TERM; printf ready > {ready}; sleep 0.4; printf survived > {survived}) & wait",
            ready = ready.display(),
            survived = survived.display(),
        );

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(&script);
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        cmd.process_group(0);

        let mut child = cmd.spawn().expect("spawn sh");
        let pid = child.id().expect("child pid") as i32;

        wait_for_file(&ready, Duration::from_secs(1)).await;

        // Empty sudo path -> skip privileged-kill branch, exercise plain killpg.
        kill_process_group(pid, Path::new("")).await;
        let _ = child.wait().await;

        sleep(Duration::from_millis(700)).await;
        assert!(
            !survived.exists(),
            "child process survived process-group kill"
        );
    }

    async fn wait_for_file(path: &std::path::Path, dur: Duration) {
        let deadline = Instant::now() + dur;
        while Instant::now() < deadline {
            if path.exists() {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {}", path.display());
    }
}
