//! Spawn/reap MCP server processes with mandatory orphan prevention.
//!
//! Unix: the child becomes its own process-group leader (`setpgid` from both
//! sides of the fork/exec race) and the whole tree is signalled via
//! `kill(-pgid, …)`. On Linux, `PR_SET_PDEATHSIG(SIGKILL)` additionally kills
//! direct children even if MCPanel is SIGKILLed and no cleanup code runs.
//! Windows: a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` tied to the
//! child covers the tree.
//!
//! PDEATHSIG fires when the spawning *thread* dies — call [`spawn`] only from
//! long-lived runtime threads, never from `spawn_blocking` workers.
//!
//! Residual gap (accepted): if MCPanel itself is SIGKILLed on Unix, the
//! group-kill cleanup never runs and PDEATHSIG only reaches *direct*
//! children — a grandchild (`npx` → `node`) is reparented and survives.
//! Unix has no Job Object equivalent short of a watchdog helper process;
//! Windows genuinely covers this case via KILL_ON_JOB_CLOSE.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};
use tracing::{debug, warn};

use crate::error::AppResult;

/// Graceful-stop window between SIGTERM / CTRL_BREAK and the hard kill.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Default)]
pub struct ProcessConfig {
    pub command: String,
    pub args: Vec<String>,
    /// Fully resolved values — secret references are resolved just-in-time
    /// by the caller (T9); nothing here is ever logged.
    pub env: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
}

/// Cloneable kill handle, deliberately separate from [`Child`]: the
/// exit-waiter task owns the `Child` for `wait()`, while stop paths only need
/// this handle.
#[derive(Clone)]
pub struct KillHandle {
    #[cfg(unix)]
    pgid: i32,
    #[cfg(windows)]
    job: std::sync::Arc<job::JobObject>,
    #[cfg(windows)]
    pid: u32,
}

impl KillHandle {
    /// Ask the whole tree to exit: SIGTERM on Unix, CTRL_BREAK on Windows.
    ///
    /// Windows caveat (unverified on real hardware — CI only compiles this):
    /// `GenerateConsoleCtrlEvent` only reaches processes sharing the
    /// caller's console, and a GUI-subsystem Tauri app has none, so this is
    /// likely a silent no-op there and stops degrade to the grace period
    /// followed by `TerminateJobObject`. Revisit on a real Windows box.
    pub fn signal_graceful(&self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-self.pgid, libc::SIGTERM);
        }
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent(
                windows_sys::Win32::System::Console::CTRL_BREAK_EVENT,
                self.pid,
            );
        }
    }

    /// Kill the whole tree immediately — grandchildren (`npx` → `node`)
    /// included.
    pub fn kill_now(&self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-self.pgid, libc::SIGKILL);
        }
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.job.raw(), 1);
        }
    }
}

pub struct ManagedChild {
    pub pid: u32,
    /// Stdio is piped; stream tasks (T4) and the protocol client (T5) take
    /// the handles from here.
    pub child: Child,
    pub kill: KillHandle,
}

impl ManagedChild {
    /// Graceful stop per spec: signal, wait [`SHUTDOWN_GRACE`], then hard-kill
    /// and reap.
    ///
    /// Test-facing convenience only: production stops go through
    /// `commands::lifecycle::stop`, which re-implements this policy against
    /// the `exited` watch because the exit-waiter task owns the `Child`
    /// there. Keep the two sequences aligned when changing either.
    pub async fn shutdown(&mut self) -> AppResult<()> {
        self.kill.signal_graceful();
        match tokio::time::timeout(SHUTDOWN_GRACE, self.child.wait()).await {
            Ok(status) => {
                debug!(target: "app::process", pid = self.pid, status = ?status?, "exited gracefully");
            }
            Err(_) => {
                warn!(target: "app::process", pid = self.pid, "grace period elapsed, killing tree");
                self.kill.kill_now();
                self.child.wait().await?;
            }
        }
        Ok(())
    }
}

/// Spawn an MCP server under supervision. Must be called from within a tokio
/// runtime, on a long-lived runtime thread (see module docs re PDEATHSIG).
pub fn spawn(config: &ProcessConfig) -> AppResult<ManagedChild> {
    let mut command = Command::new(&config.command);
    command
        .args(&config.args)
        .envs(&config.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Belt-and-braces for normal drop paths; the kill handle is the real
        // orphan prevention.
        .kill_on_drop(true);
    if let Some(cwd) = &config.cwd {
        command.current_dir(cwd);
    }

    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            // Child side of the setpgid race; the parent mirrors it below —
            // whichever runs first wins.
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(target_os = "linux")]
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    #[cfg(windows)]
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP);

    let child = command.spawn()?;
    let pid = child
        .id()
        .ok_or_else(|| std::io::Error::other("spawned child has no pid"))?;

    #[cfg(unix)]
    let kill = {
        // Parent side of the race; if the child already ran setpgid this is a
        // harmless no-op (or EACCES after exec — the group exists either way).
        unsafe { libc::setpgid(pid as i32, pid as i32) };
        KillHandle { pgid: pid as i32 }
    };
    #[cfg(windows)]
    let kill = {
        let job = job::JobObject::kill_on_close()?;
        job.assign(&child)?;
        KillHandle {
            job: std::sync::Arc::new(job),
            pid,
        }
    };

    debug!(target: "app::process", pid, command = %config.command, "spawned");
    Ok(ManagedChild { pid, child, kill })
}

#[cfg(windows)]
mod job {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    /// Owned Job Object handle; closing it (drop) kills every assigned
    /// process thanks to `KILL_ON_JOB_CLOSE`.
    pub struct JobObject(HANDLE);

    // HANDLE is a raw pointer; the Job Object APIs used here are thread-safe.
    unsafe impl Send for JobObject {}
    unsafe impl Sync for JobObject {}

    impl JobObject {
        pub fn kill_on_close() -> std::io::Result<Self> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err(std::io::Error::last_os_error());
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&info).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    let err = std::io::Error::last_os_error();
                    CloseHandle(handle);
                    return Err(err);
                }
                Ok(Self(handle))
            }
        }

        pub fn assign(&self, child: &tokio::process::Child) -> std::io::Result<()> {
            let raw = child
                .raw_handle()
                .ok_or_else(|| std::io::Error::other("child has no process handle"))?;
            let ok = unsafe { AssignProcessToJobObject(self.0, raw as HANDLE) };
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }

        pub fn raw(&self) -> HANDLE {
            self.0
        }
    }

    impl Drop for JobObject {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }
}
