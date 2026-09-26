//! Where a server's command actually comes from.
//!
//! Two failure modes dominate "my server won't start" reports across every
//! MCP client: the command isn't on the PATH a GUI app inherits (macOS and
//! Linux desktop launches don't source the login shell, so `nvm`, `uv`,
//! Homebrew and friends are invisible), and the resulting error is an opaque
//! `No such file or directory (os error 2)` that names nothing. This module
//! fixes both: it merges the login shell's PATH into the one a child sees,
//! and it resolves the command *before* spawning so a miss is reported as
//! "command not found: npx" with what was searched.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The login shell's PATH, once probed. Read by every spawn; written once
/// by [`warm_login_shell_path`].
static LOGIN_PATH: OnceLock<Option<String>> = OnceLock::new();

/// Probe the login shell's PATH once, blocking up to the bound. Call from
/// the blocking pool at launch (before the auto-start sweep), never from a
/// runtime thread: a slow `.zshrc` must not stall the event loop.
pub fn warm_login_shell_path() {
    #[cfg(unix)]
    LOGIN_PATH.get_or_init(probe_login_shell_path);
    #[cfg(not(unix))]
    LOGIN_PATH.get_or_init(|| None);
}

/// PATH as the user's login shell sees it, if the probe has run and
/// succeeded. Spawns before the probe finishes (or on Windows, or when it
/// failed) fall back to the process PATH alone.
fn login_shell_path() -> Option<&'static str> {
    LOGIN_PATH.get().and_then(Option::as_deref)
}

#[cfg(unix)]
fn probe_login_shell_path() -> Option<String> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty())?;
    // Interactive + login: `.zshrc`/`.bashrc` is where nvm et al. live, and
    // they only load for interactive shells. stdin is /dev/null so nothing
    // can wait on a terminal.
    let mut child = Command::new(&shell)
        .args(["-ilc", "printf '%s' \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return None,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::debug!(target: "app::launch", "login shell PATH probe timed out");
                return None;
            }
        }
    }
    let mut stdout = child.stdout.take()?;
    let mut out = String::new();
    std::io::Read::read_to_string(&mut stdout, &mut out).ok()?;
    let path = out.trim().to_string();
    (!path.is_empty()).then_some(path)
}

const PATH_SEPARATOR: char = if cfg!(windows) { ';' } else { ':' };

/// The PATH a child should see: an explicit `PATH` in the server's env wins
/// outright; otherwise the login shell's entries followed by this process's
/// own, deduplicated in order. Pure over its inputs so it is testable.
pub fn merge_paths(explicit: Option<&str>, login: Option<&str>, process: Option<&str>) -> String {
    if let Some(explicit) = explicit {
        return explicit.to_string();
    }
    let mut seen = Vec::new();
    for source in [login, process].into_iter().flatten() {
        for dir in source.split(PATH_SEPARATOR) {
            if !dir.is_empty() && !seen.iter().any(|s| s == dir) {
                seen.push(dir.to_string());
            }
        }
    }
    seen.join(&PATH_SEPARATOR.to_string())
}

/// The PATH to launch with, given the server's own env overrides.
pub fn effective_path(env_path: Option<&str>) -> String {
    let process = std::env::var("PATH").ok();
    merge_paths(env_path, login_shell_path(), process.as_deref())
}

/// Executable extensions tried on Windows when the command has none —
/// `npx` is really `npx.cmd`, which `CreateProcess` alone won't find.
#[cfg(windows)]
fn path_extensions() -> Vec<String> {
    std::env::var("PATHEXT")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string())
        .split(';')
        .filter(|ext| !ext.is_empty())
        .map(|ext| ext.to_ascii_lowercase())
        .collect()
}

fn is_executable_file(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Candidates for one directory entry: the name as given, plus (Windows)
/// each PATHEXT extension when the name has none.
fn candidates(dir: &Path, command: &str) -> Vec<PathBuf> {
    let plain = dir.join(command);
    #[cfg(windows)]
    if Path::new(command).extension().is_none() {
        return std::iter::once(plain.clone())
            .chain(
                path_extensions()
                    .into_iter()
                    .map(|ext| dir.join(format!("{command}{ext}"))),
            )
            .collect();
    }
    vec![plain]
}

/// Locate `command` the way a shell would, against an explicit PATH.
///
/// A command with a path separator is taken literally (relative to `cwd`
/// when relative); a bare name is searched directory by directory. The
/// error is the user-facing message: it names the command and says where
/// it was looked for, which is exactly what a bare ENOENT never does.
pub fn resolve_command(command: &str, path: &str, cwd: Option<&Path>) -> Result<PathBuf, String> {
    let given = Path::new(command);
    let has_separator = command.contains('/') || (cfg!(windows) && command.contains('\\'));
    if has_separator {
        let base = if given.is_absolute() {
            given.to_path_buf()
        } else {
            cwd.map(|c| c.join(given))
                .unwrap_or_else(|| given.to_path_buf())
        };
        for candidate in candidates(base.parent().unwrap_or(Path::new("")), &file_name(&base)) {
            if is_executable_file(&candidate) {
                return Ok(candidate);
            }
        }
        return Err(if base.exists() {
            format!("command is not executable: {}", base.display())
        } else {
            format!("command not found: {}", base.display())
        });
    }

    let dirs: Vec<&str> = path
        .split(PATH_SEPARATOR)
        .filter(|d| !d.is_empty())
        .collect();
    for dir in &dirs {
        for candidate in candidates(Path::new(dir), command) {
            if is_executable_file(&candidate) {
                return Ok(candidate);
            }
        }
    }
    Err(format!(
        "command not found: {command} — not in any of the {} PATH directories MCPanel can see \
         (if it lives in a shell-managed toolchain such as nvm or uv, set PATH in this \
         server's env, or give the full path)",
        dirs.len()
    ))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_prefers_explicit_then_login_then_process_without_duplicates() {
        let sep = PATH_SEPARATOR;
        let login = format!("/opt/nvm/bin{sep}/usr/bin");
        let process = format!("/usr/bin{sep}/bin{sep}");
        assert_eq!(
            merge_paths(None, Some(&login), Some(&process)),
            format!("/opt/nvm/bin{sep}/usr/bin{sep}/bin")
        );
        assert_eq!(
            merge_paths(Some("/only"), Some(&login), Some(&process)),
            "/only"
        );
        assert_eq!(merge_paths(None, None, None), "");
    }

    #[cfg(unix)]
    #[test]
    fn resolves_bare_names_on_path_and_explains_misses() {
        let dir = std::env::temp_dir().join(format!("mcpanel-launch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("mcp-thing");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        let plain = dir.join("notes.txt");
        std::fs::write(&plain, "").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = format!("/nonexistent{}{}", PATH_SEPARATOR, dir.display());

        assert_eq!(resolve_command("mcp-thing", &path, None).unwrap(), exe);
        let miss = resolve_command("mcp-nothing", &path, None).unwrap_err();
        assert!(miss.starts_with("command not found: mcp-nothing"), "{miss}");
        assert!(miss.contains("2 PATH directories"), "{miss}");
        let not_exec = resolve_command("notes.txt", &path, None).unwrap_err();
        assert!(not_exec.contains("command not found"), "{not_exec}");

        // Paths are taken literally, relative ones against cwd.
        assert_eq!(
            resolve_command(&exe.display().to_string(), "", None).unwrap(),
            exe
        );
        assert_eq!(
            resolve_command("./mcp-thing", "", Some(&dir)).unwrap(),
            dir.join("./mcp-thing")
        );
        let literal_plain = resolve_command(&plain.display().to_string(), "", None).unwrap_err();
        assert!(literal_plain.contains("not executable"), "{literal_plain}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
