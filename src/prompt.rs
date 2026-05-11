use std::process::Command;

use anyhow::{anyhow, Result};

pub fn prompt_password(message: &str) -> Result<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        prompt_darwin(message)
    }
    #[cfg(target_os = "linux")]
    {
        prompt_linux(message)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = message;
        Err(anyhow!("unsupported OS {}", std::env::consts::OS))
    }
}

#[cfg(target_os = "macos")]
fn prompt_darwin(message: &str) -> Result<Vec<u8>> {
    // AppleScript string literals: backslash and double-quote need escaping.
    // Real newlines pass through fine.
    let escaped = message.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "display dialog \"{escaped}\" default answer \"\" with hidden answer with title \"Claude Code sudo-mcp\" with icon caution\ntext returned of result"
    );
    let output = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "osascript exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

#[cfg(target_os = "linux")]
fn prompt_linux(message: &str) -> Result<Vec<u8>> {
    // Try GUI askpass programs in priority order. Each prints the password on
    // stdout and exits 0 on success, non-zero on cancel.
    let zenity_text = format!("--text={message}");
    let candidates: [(&str, Vec<&str>); 4] = [
        ("ssh-askpass", vec![message]),
        ("ksshaskpass", vec![message]),
        (
            "zenity",
            vec!["--password", "--title=Claude Code sudo-mcp", &zenity_text],
        ),
        (
            "kdialog",
            vec!["--title", "Claude Code sudo-mcp", "--password", message],
        ),
    ];

    for (bin, args) in candidates {
        if which(bin).is_none() {
            continue;
        }
        let output = Command::new(bin).args(&args).output()?;
        if !output.status.success() {
            return Err(anyhow!("{bin} exited {}", output.status));
        }
        return Ok(output.stdout);
    }
    Err(anyhow!(
        "no askpass program found (install ssh-askpass, zenity, or kdialog)"
    ))
}

#[cfg(target_os = "linux")]
fn which(bin: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
