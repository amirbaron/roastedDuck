//! Credential-process lifetime and JSON output. The SDK's process
//! provider leaves its child running when its future is cancelled; this adapter
//! owns the process until completion and terminates it on cancellation.
use std::process::Stdio;
use std::time::SystemTime;

use aws_credential_types::Credentials;
use tokio::io::AsyncReadExt;

struct Child {
    process: tokio::process::Child,
}

impl Drop for Child {
    fn drop(&mut self) {
        if let Some(pid) = self.process.id() {
            #[cfg(not(unix))]
            let _ = pid;
            #[cfg(unix)]
            if let Ok(pid) = i32::try_from(pid) {
                // SAFETY: the child was started in its own process group, whose
                // ID is this still-owned child's PID. An unreaped child cannot
                // have its PID reused by an unrelated process.
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
            let _ = self.process.start_kill();
            // Reap promptly even when the surrounding async runtime is itself
            // being dropped after a timeout. Tokio remains the final fallback.
            for _ in 0..100 {
                if self.process.try_wait().ok().flatten().is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }
}

pub(crate) async fn credentials(command: &str) -> Option<Credentials> {
    #[cfg(unix)]
    let shell = {
        use std::os::unix::process::CommandExt as _;
        let mut shell = std::process::Command::new("sh");
        shell.args(["-c", command]).process_group(0);
        shell
    };
    #[cfg(windows)]
    let shell = {
        use std::os::windows::process::CommandExt as _;
        let mut shell = std::process::Command::new("cmd.exe");
        shell.args(["/C"]).raw_arg(format!("\"{command}\""));
        shell
    };
    let process = tokio::process::Command::from(shell)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut child = Child { process };
    let mut stdout = child.process.stdout.take()?;
    let mut bytes = Vec::new();
    stdout.read_to_end(&mut bytes).await.ok()?;
    if !child.process.wait().await.ok()?.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    if value.get("Version")?.as_u64() != Some(1) {
        return None;
    }
    let access = value.get("AccessKeyId")?.as_str()?;
    let secret = value.get("SecretAccessKey")?.as_str()?;
    if access.is_empty() || secret.is_empty() {
        return None;
    }
    let expiry = match value.get("Expiration") {
        Some(value) => Some(
            SystemTime::try_from(
                aws_smithy_types::DateTime::from_str(
                    value.as_str()?,
                    aws_smithy_types::date_time::Format::DateTimeWithOffset,
                )
                .ok()?,
            )
            .ok()?,
        ),
        None => None,
    };
    if expiry.is_some_and(|expiry| expiry <= SystemTime::now()) {
        return None;
    }
    Some(Credentials::new(
        access,
        secret,
        value
            .get("SessionToken")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        expiry,
        "roastedDuck-process",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[cfg(unix)]
    #[test]
    fn valid_large_process_output_is_not_truncated() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let command = r#"printf '{"Version":1,"AccessKeyId":"key","SecretAccessKey":"secret","SessionToken":"'; head -c 1048577 /dev/zero | tr '\000' x; printf '"}'"#;
        let result = runtime.block_on(credentials(command)).unwrap();
        assert_eq!(result.session_token().unwrap().len(), 1_048_577);
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_terminates_the_process_group() {
        let path = std::env::temp_dir().join(format!(
            "aws-child-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        let command = format!(
            "sleep 30 & echo $! > '{}'; wait",
            path.to_string_lossy().replace('\'', "'\\''")
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(
            runtime
                .block_on(async {
                    tokio::time::timeout(Duration::from_millis(200), credentials(&command)).await
                })
                .is_err()
        );
        let pid: i32 = std::fs::read_to_string(&path).unwrap().trim().parse().unwrap();
        let mut stopped = false;
        for _ in 0..100 {
            // SAFETY: signal zero only checks existence of the recorded fixture process.
            if unsafe { libc::kill(pid, 0) } != 0 {
                stopped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        std::fs::remove_file(path).unwrap();
        assert!(stopped, "credential-process descendant survived cancellation");
    }
}
