//! Minimal `bubblewrap` wrapper for external-tool invocations.
//!
//! mat2 runs ffmpeg / exiftool inside bubblewrap to contain maliciously
//! crafted inputs from escaping to the host. We do the same for
//! `ffmpeg` and `ffprobe`. The wrapper is best-effort:
//!
//! 1. If `bwrap` isn't on the user's `PATH`, we log once and fall
//!    through to a normal `Command::new` invocation. Users on distros
//!    without bubblewrap (e.g., some minimal containers) shouldn't
//!    lose functionality.
//! 2. The sandbox is scoped to what ffmpeg actually needs: a read-only
//!    bind of `/usr` (for libs + the binary), a writable temp dir, a
//!    read-only bind of the input path, and a writable bind of the
//!    output path's *parent* directory so ffmpeg can create the file.
//!
//! All I/O goes through argv - no env vars - to minimize accidental
//! data leakage.
//!
//! Bind paths are used verbatim, without `canonicalize()`. The caller
//! appends the *same* raw path to the argv of the wrapped tool, so the
//! destination of each `--ro-bind` / `--bind` must match byte-for-byte
//! what the tool will try to open inside the sandbox. Canonicalizing
//! the dest would resolve host-side symlinks and emit a real-path
//! destination that doesn't match the symlinked path the tool is
//! asked to open, producing an ENOENT inside the sandbox. bwrap
//! already resolves the bind *source* at open time, so host-side
//! symlinks on the source side are followed transparently; no
//! user-space canonicalization is needed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Cached "is bwrap available" lookup. `OnceLock` so we don't re-probe
/// the filesystem on every call.
fn bwrap_path() -> Option<&'static str> {
    static BWRAP: OnceLock<Option<String>> = OnceLock::new();
    BWRAP
        .get_or_init(|| {
            let candidates = ["/usr/bin/bwrap", "/usr/local/bin/bwrap", "/bin/bwrap"];
            for p in candidates {
                if std::path::Path::new(p).exists() {
                    return Some(p.to_string());
                }
            }
            // PATH lookup as a last resort.
            which("bwrap")
        })
        .as_deref()
}

fn which(cmd: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return candidate.into_os_string().into_string().ok();
        }
    }
    None
}

/// Build a `Command` that runs `program` under bubblewrap if available,
/// falling back to a direct `Command::new(program)`.
///
/// `input_path` is bound read-only inside the sandbox; its *parent*
/// remains inaccessible. `output_path`'s parent directory is bound
/// read-write so the tool can create the output file. Additional
/// argv items must be appended by the caller after this returns.
///
/// Both paths should be absolute. The caller MUST use these same paths
/// verbatim in the argv of the wrapped tool so the bind destinations
/// match what the tool opens inside the sandbox.
pub fn sandboxed_command(
    program: &str,
    input_path: &Path,
    output_path: &Path,
) -> Command {
    let Some(bwrap) = bwrap_path() else {
        log::debug!(
            "bwrap not found; running {program} without sandbox. \
             Install bubblewrap for defense-in-depth."
        );
        return Command::new(program);
    };

    let output_parent: PathBuf = output_path
        .parent()
        .map_or_else(|| PathBuf::from("/tmp"), Path::to_path_buf);

    let mut cmd = Command::new(bwrap);
    cmd.args([
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--clearenv",
        "--setenv",
        "PATH",
        "/usr/bin:/bin",
        "--ro-bind",
        "/usr",
        "/usr",
        // /bin, /lib, /lib64 on systems where they are real dirs (non-usrmerge)
        "--ro-bind-try",
        "/bin",
        "/bin",
        "--ro-bind-try",
        "/lib",
        "/lib",
        "--ro-bind-try",
        "/lib64",
        "/lib64",
        "--ro-bind-try",
        "/etc/alternatives",
        "/etc/alternatives",
        "--ro-bind-try",
        "/etc/ld.so.cache",
        "/etc/ld.so.cache",
        "--ro-bind-try",
        "/etc/ld.so.conf",
        "/etc/ld.so.conf",
        "--ro-bind-try",
        "/etc/ld.so.conf.d",
        "/etc/ld.so.conf.d",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
    ]);
    cmd.arg("--ro-bind").arg(input_path).arg(input_path);
    cmd.arg("--bind").arg(&output_parent).arg(&output_parent);
    // The actual program
    cmd.arg("--");
    cmd.arg(program);
    cmd
}

/// Run `program` under a bubblewrap sandbox *without* any bind mounts
/// for specific paths. Used by probe-only tools (e.g. `ffprobe -show_format`)
/// that only need read access. The input path is bound read-only. As
/// with `sandboxed_command`, the caller must use `input_path` verbatim
/// in the tool's argv so the bind destination matches.
pub fn sandboxed_probe_command(program: &str, input_path: &Path) -> Command {
    let Some(bwrap) = bwrap_path() else {
        log::debug!("bwrap not found; running {program} without sandbox.");
        return Command::new(program);
    };

    let mut cmd = Command::new(bwrap);
    cmd.args([
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--clearenv",
        "--setenv",
        "PATH",
        "/usr/bin:/bin",
        "--ro-bind",
        "/usr",
        "/usr",
        "--ro-bind-try",
        "/bin",
        "/bin",
        "--ro-bind-try",
        "/lib",
        "/lib",
        "--ro-bind-try",
        "/lib64",
        "/lib64",
        "--ro-bind-try",
        "/etc/ld.so.cache",
        "/etc/ld.so.cache",
        "--ro-bind-try",
        "/etc/ld.so.conf",
        "/etc/ld.so.conf",
        "--ro-bind-try",
        "/etc/ld.so.conf.d",
        "/etc/ld.so.conf.d",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
    ]);
    cmd.arg("--ro-bind").arg(input_path).arg(input_path);
    cmd.arg("--");
    cmd.arg(program);
    cmd
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn bwrap_path_returns_option() {
        // Either bwrap is on this system or it isn't — both are fine.
        let _ = bwrap_path();
    }

    #[test]
    fn sandboxed_command_is_constructable_even_without_bwrap() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.mp4");
        let output = dir.path().join("out.mp4");
        std::fs::write(&input, b"dummy").unwrap();
        let cmd = sandboxed_command("ffmpeg", &input, &output);
        // Program name matches the binary we called (bwrap if present,
        // otherwise ffmpeg directly).
        let prog = cmd.get_program().to_string_lossy().into_owned();
        assert!(prog == "ffmpeg" || prog.ends_with("bwrap"));
    }
}
