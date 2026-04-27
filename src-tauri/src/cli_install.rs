// Install the bundled `runner` CLI into `$APPDATA/runner/bin/runner` at
// app startup so child PTYs find it on PATH (arch §5.3 Layer 2).
//
// Source resolution: in dev (`cargo run`), the binary built by the
// workspace lives next to the Tauri exe under `target/{debug,release}/`.
// In production, Tauri bundles `runner` as an "external binary" the
// installer drops next to the app's main executable. Either way, the
// source is `<sibling-of-current-exe>/runner` (or `runner.exe` on
// Windows). If neither exists, we log and skip — the app stays usable;
// only `runner` invocations from inside spawned sessions will fail.
//
// Skip-if-current optimization: hash-comparing the file would be slower
// than just rewriting on every startup. Compare (size, mtime) instead —
// if the source file's mtime is older or equal to the destination's
// AND sizes match, skip the copy.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

const BIN_NAME: &str = if cfg!(windows) {
    "runner.exe"
} else {
    "runner"
};

pub fn install_runner_cli(app_data_dir: &Path) -> Result<()> {
    let Some(source) = locate_source()? else {
        eprintln!(
            "runner: bundled CLI not found next to current_exe; skipping install. \
             Sessions that invoke `runner` will error until the binary is on PATH."
        );
        return Ok(());
    };
    let dest_dir = app_data_dir.join("bin");
    std::fs::create_dir_all(&dest_dir)?;
    let dest = dest_dir.join(BIN_NAME);

    if up_to_date(&source, &dest)? {
        return Ok(());
    }

    // Copy via tempfile + rename to keep the swap atomic — a half-written
    // file would crash the next agent that runs `runner help`.
    let tmp = tempfile::NamedTempFile::new_in(&dest_dir)?;
    std::fs::copy(&source, tmp.path())?;
    tmp.persist(&dest).map_err(|e| Error::Io(e.error))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest, perms)?;
    }
    Ok(())
}

fn locate_source() -> Result<Option<PathBuf>> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| Error::msg("current_exe has no parent"))?;
    let candidate = dir.join(BIN_NAME);
    if candidate.exists() && candidate != exe {
        return Ok(Some(candidate));
    }
    Ok(None)
}

fn up_to_date(source: &Path, dest: &Path) -> Result<bool> {
    let Ok(dst_meta) = std::fs::metadata(dest) else {
        return Ok(false);
    };
    let src_meta = std::fs::metadata(source)?;
    if src_meta.len() != dst_meta.len() {
        return Ok(false);
    }
    let src_mtime = src_meta.modified().ok();
    let dst_mtime = dst_meta.modified().ok();
    match (src_mtime, dst_mtime) {
        (Some(s), Some(d)) => Ok(s <= d),
        _ => Ok(false),
    }
}
