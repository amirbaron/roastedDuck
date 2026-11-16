//! Own the Azure CLI process group until output and exit are both observed.
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::{check_interrupt, io_error};
use roastedduck_common::Result;

struct OwnedChild {
    child: Child,
    reaped: bool,
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.reaped {
            #[cfg(unix)]
            if let Ok(pid) = i32::try_from(self.child.id()) {
                // SAFETY: this unreaped child owns the process group created below;
                // its PID cannot be reused while we still hold the child.
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub(super) fn token() -> Result<Vec<u8>> {
    #[cfg(unix)]
    let mut command = {
        use std::os::unix::process::CommandExt as _;
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "az account get-access-token --output json --scope \"https://storage.azure.com/.default\"",
        ]);
        command.process_group(0);
        command
    };
    #[cfg(not(unix))]
    let mut command = {
        let mut command = Command::new("cmd.exe");
        command.args([
            "/C",
            "az account get-access-token --output json --scope \"https://storage.azure.com/.default\"",
        ]);
        command
    };
    let mut path = std::env::var("PATH").unwrap_or_default();
    #[cfg(unix)]
    {
        if !path.is_empty() {
            path.push(':');
        }
        path.push_str("/usr/bin:/usr/local/bin");
    }
    #[cfg(windows)]
    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(directory) = std::env::var(variable) {
            if !path.is_empty() {
                path.push(';');
            }
            path.push_str(&format!("{directory}\\Microsoft SDKs\\Azure\\CLI2\\wbin"));
        }
    }
    command.env_clear().env("PATH", path);
    #[cfg(windows)]
    for variable in ["SystemRoot", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(variable) {
            command.env(variable, value);
        }
    }
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(io_error)?;
    let mut child = OwnedChild { child, reaped: false };
    let mut stdout = child
        .child
        .stdout
        .take()
        .ok_or_else(|| io_error("Azure CLI stdout is missing"))?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.read_to_end(&mut bytes).map(|_| bytes);
        let _ = sender.send(result);
    });
    let started = Instant::now();
    let mut output = None;
    loop {
        check_interrupt()?;
        if started.elapsed() >= Duration::from_secs(13) {
            return Err(io_error("AzureCliCredential process timed out"));
        }
        if let Ok(result) = receiver.try_recv() {
            output = Some(result.map_err(io_error)?);
        }
        if output.is_some() && child.child.try_wait().map_err(io_error)?.is_some() {
            child.reaped = true;
            // The pinned SDK parses stdout even if the process exits nonzero.
            return Ok(output.take().unwrap_or_default());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
