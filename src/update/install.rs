//! Self-update of an `install.sh` install: download, verify, extract, replace.
//!
//! Mirrors install.sh step for step (same paths, same files, same atomic
//! `.<name>.new.<pid>` + rename), with a rollback if a rename fails halfway.
//! Everything is written next to the targets first (same filesystem, so the
//! renames are atomic); any failure leaves the current install untouched.
//!
//! Windows (a folder install from the release `.zip`): a running `.exe`
//! cannot be overwritten or deleted, but it can be renamed. Each program is
//! moved aside to `.<name>.exe.old.<pid>` and the new one renamed into its
//! place ([`replace_moving`]); the old copies are deleted on the next start
//! ([`clean_leftovers`]), once nothing runs them.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;

use sha2::{Digest, Sha256};

use super::http::{self, DownloadError, MAX_DOWNLOAD, Policy};
use super::method::ScriptLayout;
use super::release::Package;
use super::{APP_ID, CRATE, HELPERS};

/// Largest `.sha256` file.
const MAX_CHECKSUM_FILE: u64 = 4096;
/// Largest extracted size.
pub const MAX_EXTRACT: u64 = 1024 * 1024 * 1024;
/// Most entries in the archive.
const MAX_ENTRIES: usize = 10_000;
/// Prefix of the work folder inside the bin dir.
const WORK_PREFIX: &str = ".ferrochat-update.";

/// What the installer is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Bytes received / expected.
    Downloading {
        /// Received so far.
        received: u64,
        /// `Content-Length`, when sent.
        total: Option<u64>,
    },
    /// Checking the SHA-256.
    Verifying,
    /// Extracting and replacing files.
    Installing,
}

/// Why an install stopped. The current install is untouched in every case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// The user cancelled the download.
    Cancelled,
    /// Anything else, with a message for the user.
    Failed(String),
}

impl From<DownloadError> for InstallError {
    fn from(e: DownloadError) -> Self {
        match e {
            DownloadError::Cancelled => Self::Cancelled,
            DownloadError::Failed(m) => Self::Failed(m),
        }
    }
}

impl From<String> for InstallError {
    fn from(m: String) -> Self {
        Self::Failed(m)
    }
}

/// Downloads `package` and installs it into `layout`.
pub fn install(
    layout: &ScriptLayout,
    package: &Package,
    policy: &Policy,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Stage),
) -> Result<(), InstallError> {
    let work = WorkDir::create(&layout.bin_dir)?;

    let mut sum = Vec::new();
    http::download(
        &package.checksum.url,
        policy,
        &mut sum,
        MAX_CHECKSUM_FILE,
        cancel,
        &mut |_, _| {},
    )?;
    let expected = parse_checksum(&String::from_utf8_lossy(&sum), &package.archive.name)?;

    let archive = work.path.join(if layout.windows {
        "release.zip"
    } else {
        "release.tar.gz"
    });
    let file = File::create(&archive).map_err(|e| io_msg("create the download", &e))?;
    let mut out = HashingWriter {
        inner: BufWriter::new(file),
        hasher: Sha256::new(),
    };
    progress(Stage::Downloading {
        received: 0,
        total: (package.archive.size > 0).then_some(package.archive.size),
    });
    http::download(
        &package.archive.url,
        policy,
        &mut out,
        MAX_DOWNLOAD,
        cancel,
        &mut |received, total| progress(Stage::Downloading { received, total }),
    )?;
    let actual = out.finish().map_err(|e| io_msg("save the download", &e))?;

    progress(Stage::Verifying);
    verify(&expected, &actual)?;

    progress(Stage::Installing);
    install_archive(layout, &archive, &work.path, &package.version.to_string())?;
    tracing::info!(version = %package.version, "update installed");
    Ok(())
}

/// Compares hex digests.
pub fn verify(expected: &str, actual: &str) -> Result<(), String> {
    if expected.eq_ignore_ascii_case(actual) {
        Ok(())
    } else {
        tracing::warn!("update checksum mismatch; nothing was installed");
        Err("The download is damaged (checksum mismatch); nothing was installed.".into())
    }
}

/// Reads `sha256sum` output: `<64 hex>  <name>` (the name, when present,
/// must be the archive's).
pub fn parse_checksum(text: &str, archive_name: &str) -> Result<String, String> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .ok_or("the checksum file is empty")?;
    let mut parts = line.split_whitespace();
    let hash = parts.next().unwrap_or_default();
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("the checksum file is not valid".into());
    }
    if let Some(name) = parts.next() {
        let name = name.strip_prefix('*').unwrap_or(name);
        if name != archive_name {
            return Err("the checksum file is for another file".into());
        }
    }
    Ok(hash.to_ascii_lowercase())
}

/// Hex SHA-256 of a file.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

struct HashingWriter {
    inner: BufWriter<File>,
    hasher: Sha256,
}

impl HashingWriter {
    fn finish(mut self) -> std::io::Result<String> {
        self.inner.flush()?;
        self.inner.get_ref().sync_all()?;
        Ok(hex::encode(self.hasher.finalize()))
    }
}

impl Write for HashingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn io_msg(what: &str, e: &std::io::Error) -> String {
    format!("Could not {what}: {e}")
}

/// `<bin_dir>/.ferrochat-update.<pid>`, removed on drop. Creating it also
/// clears what an interrupted earlier update left behind.
struct WorkDir {
    path: PathBuf,
}

impl WorkDir {
    fn create(bin_dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(bin_dir).map_err(|e| io_msg("create the install folder", &e))?;
        clean_stale(bin_dir);
        let path = bin_dir.join(format!("{WORK_PREFIX}{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).map_err(|e| io_msg("write to the install folder", &e))?;
        Ok(Self { path })
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Removes what earlier updates left in `bin_dir`: temp files of updates
/// whose process is gone and, on Windows, the moved-aside old programs
/// (deleting one that still runs fails and is retried next time).
pub fn clean_leftovers(bin_dir: &Path) {
    clean_stale(bin_dir);
}

/// Removes temp files of updates whose process is gone.
fn clean_stale(bin_dir: &Path) {
    let Ok(dir) = fs::read_dir(bin_dir) else {
        return;
    };
    let ours = std::process::id().to_string();
    let names: Vec<String> = std::iter::once(CRATE)
        .chain(HELPERS.iter().copied())
        .flat_map(|n| [n.to_string(), format!("{n}.exe")])
        .flat_map(|n| [format!(".{n}.new."), format!(".{n}.old.")])
        .chain([WORK_PREFIX.to_string()])
        .collect();
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = names.iter().find_map(|p| name.strip_prefix(p.as_str())) else {
            continue;
        };
        if !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        // Our own temp files are in use; our own moved-aside old program is
        // only kept while it runs (deleting it then fails).
        if pid == ours && (!name.contains(".old.") || cfg!(target_os = "linux")) {
            continue;
        }
        if leftover_in_use(&name, pid, &entry) {
            continue;
        }
        let p = entry.path();
        let _ = if p.is_dir() {
            fs::remove_dir_all(&p)
        } else {
            fs::remove_file(&p)
        };
    }
}

/// Whether a leftover of update process `pid` may still be in use. Linux
/// asks `/proc`. Elsewhere an old program is always tried (deleting a
/// running `.exe` just fails) and temp files count as in use for an hour.
fn leftover_in_use(name: &str, pid: &str, entry: &fs::DirEntry) -> bool {
    if cfg!(target_os = "linux") {
        return Path::new("/proc").join(pid).exists();
    }
    if name.contains(".old.") {
        return false;
    }
    entry
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_none_or(|age| age < std::time::Duration::from_secs(3600))
}

/// Unpacks the release archive and installs it: a `.tar.gz` like install.sh,
/// or the Windows `.zip` into its folder.
pub fn install_archive(
    layout: &ScriptLayout,
    archive: &Path,
    work: &Path,
    version: &str,
) -> Result<(), String> {
    if layout.windows {
        return install_zip(layout, archive, work, version);
    }
    let x = work.join("x");
    fs::create_dir(&x).map_err(|e| io_msg("unpack the update", &e))?;
    let f = File::open(archive).map_err(|e| io_msg("open the download", &e))?;
    safe_extract(flate2::read::GzDecoder::new(f), &x, MAX_EXTRACT)?;
    let src = single_top_dir(&x)?;
    let desktop_src = src.join("packaging").join(format!("{APP_ID}.desktop"));
    let icon_src = src.join("packaging").join(format!("{APP_ID}.svg"));
    if !src.join(CRATE).is_file() {
        return Err(format!("The update has no {CRATE} program."));
    }
    if !desktop_src.is_file() || !icon_src.is_file() {
        return Err("The update has no desktop entry or icon.".into());
    }

    // Binaries: the app plus the helpers the tarball ships (like install.sh,
    // a helper missing from an old tarball is skipped).
    let pid = std::process::id();
    let mut staged = Vec::new();
    for name in std::iter::once(CRATE).chain(HELPERS.iter().copied()) {
        let from = src.join(name);
        if !from.is_file() {
            continue;
        }
        let tmp = layout.bin_dir.join(format!(".{name}.new.{pid}"));
        if let Err(e) = stage_file(&from, &tmp, 0o755) {
            for (t, _) in &staged {
                let _ = fs::remove_file(t);
            }
            let _ = fs::remove_file(&tmp);
            return Err(io_msg("write the new program", &e));
        }
        staged.push((tmp, layout.bin(name)));
    }
    replace_all(&staged)?;

    // Desktop entry and icon, as install.sh writes them. The binaries are
    // already in place, so a failure here is only logged.
    let mut changed = false;
    match fs::read_to_string(&desktop_src) {
        Ok(shipped) => {
            let entry = desktop_entry(&shipped, &layout.bin(CRATE));
            let dest = layout.apps_dir().join(format!("{APP_ID}.desktop"));
            match write_if_changed(&dest, entry.as_bytes()) {
                Ok(c) => changed |= c,
                Err(e) => tracing::warn!(error = %e, "could not update the desktop entry"),
            }
        }
        Err(e) => tracing::warn!(error = %e, "could not read the shipped desktop entry"),
    }
    match fs::read(&icon_src) {
        Ok(svg) => match write_if_changed(&layout.icon_dir().join(format!("{APP_ID}.svg")), &svg) {
            Ok(c) => changed |= c,
            Err(e) => tracing::warn!(error = %e, "could not update the icon"),
        },
        Err(e) => tracing::warn!(error = %e, "could not read the shipped icon"),
    }
    if let Err(e) = write_if_changed(&layout.version_file(), format!("{version}\n").as_bytes()) {
        tracing::warn!(error = %e, "could not record the installed version");
    }
    if changed {
        refresh_caches(layout);
    }
    Ok(())
}

/// Unpacks the Windows `.zip` and installs its programs into the folder.
fn install_zip(
    layout: &ScriptLayout,
    archive: &Path,
    work: &Path,
    version: &str,
) -> Result<(), String> {
    let x = work.join("x");
    fs::create_dir(&x).map_err(|e| io_msg("unpack the update", &e))?;
    let f = File::open(archive).map_err(|e| io_msg("open the download", &e))?;
    safe_extract_zip(std::io::BufReader::new(f), &x, MAX_EXTRACT)?;
    let src = single_top_dir(&x)?;
    let exe = |name: &str| format!("{name}.exe");
    if !src.join(exe(CRATE)).is_file() {
        return Err(format!("The update has no {CRATE}.exe program."));
    }
    let pid = std::process::id();
    let mut staged = Vec::new();
    for name in std::iter::once(CRATE).chain(HELPERS.iter().copied()) {
        let from = src.join(exe(name));
        if !from.is_file() {
            continue;
        }
        let tmp = layout.bin_dir.join(format!(".{}.new.{pid}", exe(name)));
        if let Err(e) = stage_file(&from, &tmp, 0o755) {
            for (t, _) in &staged {
                let _ = fs::remove_file(t);
            }
            let _ = fs::remove_file(&tmp);
            return Err(io_msg("write the new program", &e));
        }
        staged.push((tmp, layout.bin(name)));
    }
    replace_moving(&staged)?;
    if let Err(e) = write_if_changed(&layout.version_file(), format!("{version}\n").as_bytes()) {
        tracing::warn!(error = %e, "could not record the installed version");
    }
    Ok(())
}

/// Replaces each target by moving it aside (`.<name>.old.<pid>`) and then
/// renaming the staged file into its place: the only way to replace a
/// program that is running on Windows. If a step fails, everything already
/// moved is put back. The moved-aside files are deleted when possible; the
/// running program's copy stays until [`clean_leftovers`] on a later start.
pub fn replace_moving(staged: &[(PathBuf, PathBuf)]) -> Result<(), String> {
    let pid = std::process::id();
    // (target, its moved-aside copy) for every target already replaced.
    let mut done: Vec<(&Path, Option<PathBuf>)> = Vec::new();
    let undo = |done: &[(&Path, Option<PathBuf>)]| {
        for (target, bak) in done.iter().rev() {
            let _ = fs::remove_file(target);
            if let Some(b) = bak {
                let _ = fs::rename(b, target);
            }
        }
        for (s, _) in staged {
            let _ = fs::remove_file(s);
        }
    };
    for (s, target) in staged {
        let bak = if fs::symlink_metadata(target).is_ok() {
            let name = target
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let bak = target.with_file_name(format!(".{name}.old.{pid}"));
            let _ = fs::remove_file(&bak);
            if let Err(e) = fs::rename(target, &bak) {
                tracing::warn!(error = %e, "update move-aside failed; rolling back");
                undo(&done);
                return Err(io_msg("replace the program (nothing was changed)", &e));
            }
            Some(bak)
        } else {
            None
        };
        if let Err(e) = fs::rename(s, target) {
            tracing::warn!(error = %e, "update rename failed; rolling back");
            if let Some(b) = &bak {
                let _ = fs::rename(b, target);
            }
            undo(&done);
            return Err(io_msg("replace the program (nothing was changed)", &e));
        }
        done.push((target, bak));
    }
    for (_, bak) in &done {
        if let Some(b) = bak {
            // Fails for the program that is running (Windows): left for later.
            let _ = fs::remove_file(b);
        }
    }
    Ok(())
}

fn stage_file(from: &Path, to: &Path, mode: u32) -> std::io::Result<()> {
    let _ = fs::remove_file(to);
    fs::copy(from, to)?;
    set_mode(to, mode)?;
    // Windows flushes only through a handle with write access.
    fs::OpenOptions::new().write(true).open(to)?.sync_all()
}

#[cfg(unix)]
fn set_mode(p: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_p: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

/// Renames each staged file over its target. If one rename fails, the
/// targets already replaced are restored from backups (hard links of the
/// old files) and the error is returned.
pub fn replace_all(staged: &[(PathBuf, PathBuf)]) -> Result<(), String> {
    let pid = std::process::id();
    let cleanup_staged = || {
        for (s, _) in staged {
            let _ = fs::remove_file(s);
        }
    };
    let mut backups: Vec<Option<PathBuf>> = Vec::new();
    for (_, target) in staged {
        if fs::symlink_metadata(target).is_err() {
            backups.push(None);
            continue;
        }
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bak = target.with_file_name(format!(".{name}.old.{pid}"));
        let _ = fs::remove_file(&bak);
        let made = fs::hard_link(target, &bak).or_else(|_| fs::copy(target, &bak).map(|_| ()));
        if let Err(e) = made {
            cleanup_staged();
            for b in backups.iter().flatten() {
                let _ = fs::remove_file(b);
            }
            return Err(io_msg("back up the current program", &e));
        }
        backups.push(Some(bak));
    }
    for (i, (s, target)) in staged.iter().enumerate() {
        if let Err(e) = fs::rename(s, target) {
            tracing::warn!(error = %e, "update rename failed; rolling back");
            for (j, (_, t)) in staged.iter().enumerate().take(i) {
                match &backups[j] {
                    Some(b) => {
                        let _ = fs::rename(b, t);
                    }
                    None => {
                        let _ = fs::remove_file(t);
                    }
                }
            }
            cleanup_staged();
            for b in backups.iter().flatten() {
                let _ = fs::remove_file(b);
            }
            return Err(io_msg("replace the program (nothing was changed)", &e));
        }
    }
    for b in backups.iter().flatten() {
        let _ = fs::remove_file(b);
    }
    Ok(())
}

/// Atomic write (temp + rename) unless the file already has these bytes.
fn write_if_changed(dest: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    if fs::read(dest).is_ok_and(|old| old == bytes) {
        return Ok(false);
    }
    let dir = dest.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(dir)?;
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.new.{}", std::process::id()));
    let res = fs::write(&tmp, bytes).and_then(|()| fs::rename(&tmp, dest));
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res.map(|()| true)
}

/// install.sh's desktop entry rewrite: every `Exec=` line becomes the
/// absolute binary path (quoted if it has a space) plus the field codes of
/// the shipped first `Exec=` line.
pub fn desktop_entry(shipped: &str, bin: &Path) -> String {
    let args = shipped
        .lines()
        .find_map(|l| l.strip_prefix("Exec="))
        .map(|rest| rest.find(' ').map_or("", |i| &rest[i..]))
        .unwrap_or("");
    let bin = bin.to_string_lossy();
    let exec_bin = if bin.contains(' ') {
        format!("\"{bin}\"")
    } else {
        bin.into_owned()
    };
    let mut out = String::with_capacity(shipped.len() + 64);
    for line in shipped.lines() {
        if line.starts_with("Exec=") {
            out.push_str("Exec=");
            out.push_str(&exec_bin);
            out.push_str(args);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// install.sh's `refresh_caches` (best effort, only when files changed).
fn refresh_caches(layout: &ScriptLayout) {
    use std::process::{Command, Stdio};
    let run = |cmd: &str, args: &[&std::ffi::OsStr]| {
        Command::new(cmd)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    };
    let apps = layout.apps_dir();
    run("update-desktop-database", &[apps.as_os_str()]);
    let hicolor = layout.data_dir.join("icons/hicolor");
    run(
        "gtk-update-icon-cache",
        &["-f".as_ref(), "-t".as_ref(), hicolor.as_os_str()],
    );
    if !run("kbuildsycoca6", &[]) {
        run("kbuildsycoca5", &[]);
    }
}

/// The one directory at the top of the unpacked archive.
fn single_top_dir(x: &Path) -> Result<PathBuf, String> {
    let mut dirs = fs::read_dir(x)
        .map_err(|e| io_msg("read the update", &e))?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path());
    match (dirs.next(), dirs.next()) {
        (Some(d), None) => Ok(d),
        _ => Err("The update archive has an unexpected layout.".into()),
    }
}

/// Unpacks a tar stream into `dest`. Rejects absolute paths, `..`, links
/// and special files; caps the total size and the entry count. Only plain
/// files (mode 0644 / 0755) and directories are created.
pub fn safe_extract(reader: impl Read, dest: &Path, cap: u64) -> Result<(), String> {
    let bad = |what: &str| format!("The update archive is not valid ({what}).");
    let mut archive = tar::Archive::new(reader);
    let mut total = 0u64;
    let entries = archive.entries().map_err(|_| bad("unreadable"))?;
    for (count, entry) in entries.enumerate() {
        if count >= MAX_ENTRIES {
            return Err(bad("too many files"));
        }
        let mut entry = entry.map_err(|_| bad("unreadable"))?;
        let kind = entry.header().entry_type();
        if matches!(
            kind,
            tar::EntryType::XGlobalHeader | tar::EntryType::XHeader | tar::EntryType::GNULongName
        ) {
            continue;
        }
        let path = entry.path().map_err(|_| bad("bad name"))?.into_owned();
        let rel = safe_relative(&path).ok_or_else(|| bad("unsafe path"))?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = dest.join(&rel);
        match kind {
            tar::EntryType::Directory => {
                fs::create_dir_all(&out).map_err(|e| io_msg("unpack the update", &e))?;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let size = entry.size();
                total = total.saturating_add(size);
                if total > cap {
                    return Err(bad("too large"));
                }
                if let Some(parent) = out.parent() {
                    fs::create_dir_all(parent).map_err(|e| io_msg("unpack the update", &e))?;
                }
                let exec = entry.header().mode().is_ok_and(|m| m & 0o111 != 0);
                let mut f = File::create(&out).map_err(|e| io_msg("unpack the update", &e))?;
                let copied = std::io::copy(&mut (&mut entry).take(size), &mut f)
                    .map_err(|_| bad("truncated"))?;
                if copied != size {
                    return Err(bad("truncated"));
                }
                drop(f);
                set_mode(&out, if exec { 0o755 } else { 0o644 })
                    .map_err(|e| io_msg("unpack the update", &e))?;
            }
            tar::EntryType::Symlink | tar::EntryType::Link => return Err(bad("contains a link")),
            _ => return Err(bad("contains a special file")),
        }
    }
    Ok(())
}

/// Unpacks a zip into `dest` with the same rules as [`safe_extract`]:
/// plain relative names only (backslashes count as separators), no links,
/// capped total size and entry count. The zip reader checks each file's CRC.
pub fn safe_extract_zip(
    reader: impl Read + std::io::Seek,
    dest: &Path,
    cap: u64,
) -> Result<(), String> {
    let bad = |what: &str| format!("The update archive is not valid ({what}).");
    let mut archive = zip::ZipArchive::new(reader).map_err(|_| bad("unreadable"))?;
    if archive.len() > MAX_ENTRIES {
        return Err(bad("too many files"));
    }
    let mut total = 0u64;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|_| bad("unreadable"))?;
        let name = String::from_utf8_lossy(entry.name_raw()).replace('\\', "/");
        if name.contains(':') {
            return Err(bad("unsafe path"));
        }
        let rel = safe_relative(Path::new(&name)).ok_or_else(|| bad("unsafe path"))?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        if entry.is_symlink() {
            return Err(bad("contains a link"));
        }
        let out = dest.join(&rel);
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(|e| io_msg("unpack the update", &e))?;
            continue;
        }
        if !entry.is_file() {
            return Err(bad("contains a special file"));
        }
        let size = entry.size();
        total = total.saturating_add(size);
        if total > cap {
            return Err(bad("too large"));
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|e| io_msg("unpack the update", &e))?;
        }
        let mut f = File::create(&out).map_err(|e| io_msg("unpack the update", &e))?;
        let copied = std::io::copy(&mut (&mut entry).take(size.saturating_add(1)), &mut f)
            .map_err(|_| bad("damaged"))?;
        if copied != size {
            return Err(bad("truncated"));
        }
        drop(f);
        let exec = name.ends_with(".exe") || entry.unix_mode().is_some_and(|m| m & 0o111 != 0);
        set_mode(&out, if exec { 0o755 } else { 0o644 })
            .map_err(|e| io_msg("unpack the update", &e))?;
    }
    Ok(())
}

/// `path` as plain relative components, or `None` if it is absolute or
/// climbs out with `..`.
fn safe_relative(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Normal(n) => out.push(n),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ferrochat-upd-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A tar with one raw header (bypassing the builder's own path checks).
    fn raw_tar(name: &str, kind: tar::EntryType, link: Option<&str>, data: &[u8]) -> Vec<u8> {
        let mut h = tar::Header::new_gnu();
        {
            let gnu = h.as_gnu_mut().unwrap();
            gnu.name[..name.len()].copy_from_slice(name.as_bytes());
        }
        h.set_entry_type(kind);
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        if let Some(l) = link {
            h.set_link_name(l).unwrap();
        }
        h.set_cksum();
        let mut out = h.as_bytes().to_vec();
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
        out.extend_from_slice(&[0u8; 1024]);
        out
    }

    #[test]
    fn extracts_plain_files_and_dirs() {
        let d = temp("ok");
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(3);
        h.set_mode(0o755);
        h.set_cksum();
        b.append_data(&mut h, "pkg/ferrochat", &b"elf"[..]).unwrap();
        let mut h = tar::Header::new_gnu();
        h.set_size(2);
        h.set_mode(0o600);
        h.set_cksum();
        b.append_data(&mut h, "./pkg/packaging/a.txt", &b"hi"[..])
            .unwrap();
        let bytes = b.into_inner().unwrap();
        safe_extract(&bytes[..], &d, 1000).unwrap();
        assert_eq!(fs::read(d.join("pkg/ferrochat")).unwrap(), b"elf");
        assert_eq!(fs::read(d.join("pkg/packaging/a.txt")).unwrap(), b"hi");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = |p: &str| fs::metadata(d.join(p)).unwrap().permissions().mode() & 0o777;
            assert_eq!(m("pkg/ferrochat"), 0o755);
            assert_eq!(m("pkg/packaging/a.txt"), 0o644);
        }
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn rejects_traversal_absolute_links_and_oversize() {
        let d = temp("bad");
        let cases = [
            raw_tar("../evil", tar::EntryType::Regular, None, b"x"),
            raw_tar("pkg/../../evil", tar::EntryType::Regular, None, b"x"),
            raw_tar("/tmp/evil", tar::EntryType::Regular, None, b"x"),
            raw_tar(
                "pkg/link",
                tar::EntryType::Symlink,
                Some("/etc/passwd"),
                b"",
            ),
            raw_tar("pkg/hard", tar::EntryType::Link, Some("/etc/passwd"), b""),
            raw_tar("pkg/fifo", tar::EntryType::Fifo, None, b""),
            raw_tar("pkg/dev", tar::EntryType::Char, None, b""),
        ];
        for (i, t) in cases.iter().enumerate() {
            assert!(safe_extract(&t[..], &d, 1000).is_err(), "case {i}");
        }
        let big = raw_tar("pkg/big", tar::EntryType::Regular, None, &[0u8; 2048]);
        assert!(safe_extract(&big[..], &d, 1000).is_err());
        assert!(!d.parent().unwrap().join("evil").exists());
        assert!(!d.join("pkg/link").exists());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn checksum_file_parsing() {
        let h = "a".repeat(64);
        assert_eq!(
            parse_checksum(&format!("{h}  x.tar.gz\n"), "x.tar.gz").unwrap(),
            h
        );
        assert_eq!(
            parse_checksum(&format!("\n{}  *x.tar.gz", h.to_uppercase()), "x.tar.gz").unwrap(),
            h
        );
        assert_eq!(parse_checksum(&h, "x.tar.gz").unwrap(), h);
        assert!(parse_checksum(&format!("{h}  y.tar.gz"), "x.tar.gz").is_err());
        assert!(parse_checksum("abc  x.tar.gz", "x.tar.gz").is_err());
        assert!(parse_checksum(&"g".repeat(64), "x.tar.gz").is_err());
        assert!(parse_checksum("", "x.tar.gz").is_err());
    }

    #[test]
    fn sha_mismatch_aborts() {
        let h = "a".repeat(64);
        assert!(verify(&h, &h.to_uppercase()).is_ok());
        assert!(verify(&h, &"b".repeat(64)).is_err());
    }

    #[test]
    fn desktop_entry_matches_install_sh() {
        let shipped = "[Desktop Entry]\nName=Ferrochat\nExec=ferrochat %U\nIcon=dev.ferro.ferrochat\n\n[Desktop Action New]\nExec=ferrochat --new\n";
        let out = desktop_entry(shipped, Path::new("/home/u/.local/bin/ferrochat"));
        assert_eq!(
            out,
            "[Desktop Entry]\nName=Ferrochat\nExec=/home/u/.local/bin/ferrochat %U\nIcon=dev.ferro.ferrochat\n\n[Desktop Action New]\nExec=/home/u/.local/bin/ferrochat %U\n"
        );
        let out = desktop_entry("Exec=ferrochat\n", Path::new("/home/my user/bin/ferrochat"));
        assert_eq!(out, "Exec=\"/home/my user/bin/ferrochat\"\n");
    }

    #[test]
    fn replace_all_rolls_back_when_a_rename_fails() {
        let d = temp("rollback");
        fs::write(d.join("ferrochat"), b"old-app").unwrap();
        // The helper's target is a non-empty directory: renaming a file over
        // it fails, after the app was already replaced.
        fs::create_dir_all(d.join("ferrochat-player/x")).unwrap();
        fs::write(d.join(".ferrochat.new.1"), b"new-app").unwrap();
        fs::write(d.join(".ferrochat-player.new.1"), b"new-player").unwrap();
        let staged = vec![
            (d.join(".ferrochat.new.1"), d.join("ferrochat")),
            (
                d.join(".ferrochat-player.new.1"),
                d.join("ferrochat-player"),
            ),
        ];
        assert!(replace_all(&staged).is_err());
        assert_eq!(fs::read(d.join("ferrochat")).unwrap(), b"old-app");
        let left: Vec<_> = fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(left.is_empty(), "temp files left: {left:?}");
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn replace_all_installs_new_targets() {
        let d = temp("replace");
        fs::write(d.join("ferrochat"), b"old").unwrap();
        fs::write(d.join(".a.new"), b"new").unwrap();
        fs::write(d.join(".b.new"), b"helper").unwrap();
        replace_all(&[
            (d.join(".a.new"), d.join("ferrochat")),
            (d.join(".b.new"), d.join("ferrochat-player")),
        ])
        .unwrap();
        assert_eq!(fs::read(d.join("ferrochat")).unwrap(), b"new");
        assert_eq!(fs::read(d.join("ferrochat-player")).unwrap(), b"helper");
        assert_eq!(fs::read_dir(&d).unwrap().count(), 2);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn old_programs_are_removed_and_fresh_temp_files_kept_off_linux() {
        let d = temp("stale-win");
        fs::write(d.join(".ferrochat.exe.old.99999999"), b"x").unwrap();
        fs::write(d.join(".ferrochat-player.exe.old.1234"), b"x").unwrap();
        fs::write(d.join(".ferrochat.exe.new.99999999"), b"x").unwrap();
        fs::write(d.join("ferrochat.exe"), b"keep").unwrap();
        clean_leftovers(&d);
        assert!(!d.join(".ferrochat.exe.old.99999999").exists());
        assert!(!d.join(".ferrochat-player.exe.old.1234").exists());
        assert!(d.join(".ferrochat.exe.new.99999999").exists());
        assert!(d.join("ferrochat.exe").exists());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn stale_temp_files_of_dead_processes_are_cleaned() {
        let d = temp("stale");
        // PIDs above the kernel maximum never exist.
        fs::create_dir_all(d.join(".ferrochat-update.99999999")).unwrap();
        fs::write(d.join(".ferrochat.new.99999999"), b"x").unwrap();
        fs::write(d.join(".ferrochat.new.notapid"), b"x").unwrap();
        fs::write(d.join("ferrochat"), b"keep").unwrap();
        let w = WorkDir::create(&d).unwrap();
        assert!(w.path.is_dir());
        assert!(!d.join(".ferrochat-update.99999999").exists());
        assert!(!d.join(".ferrochat.new.99999999").exists());
        assert!(d.join(".ferrochat.new.notapid").exists());
        drop(w);
        assert!(
            !d.join(format!("{WORK_PREFIX}{}", std::process::id()))
                .exists()
        );
        let _ = fs::remove_dir_all(d);
    }

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, data) in entries {
            if name.ends_with('/') {
                z.add_directory(*name, zip::write::SimpleFileOptions::default())
                    .unwrap();
            } else {
                z.start_file(*name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                z.write_all(data).unwrap();
            }
        }
        z.finish().unwrap().into_inner()
    }

    #[test]
    fn zip_extracts_plain_files_and_backslash_names() {
        let d = temp("zip-ok");
        let z = zip_of(&[
            ("top/", b""),
            ("top/ferrochat.exe", b"app"),
            ("top\\licenses\\a.txt", b"lic"),
        ]);
        safe_extract_zip(std::io::Cursor::new(z), &d, 1024).unwrap();
        assert_eq!(fs::read(d.join("top/ferrochat.exe")).unwrap(), b"app");
        assert_eq!(fs::read(d.join("top/licenses/a.txt")).unwrap(), b"lic");
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn zip_rejects_traversal_drive_paths_and_oversize() {
        let d = temp("zip-bad");
        for name in [
            "../evil",
            "top/../../evil",
            "/abs",
            "C:/evil",
            "top\\..\\..\\evil",
        ] {
            let z = zip_of(&[(name, b"x")]);
            let e = safe_extract_zip(std::io::Cursor::new(z), &d, 1024).unwrap_err();
            assert!(e.contains("unsafe path"), "{name}: {e}");
        }
        let z = zip_of(&[("top/big", &[0u8; 64])]);
        let e = safe_extract_zip(std::io::Cursor::new(z), &d, 10).unwrap_err();
        assert!(e.contains("too large"), "{e}");
        assert!(safe_extract_zip(std::io::Cursor::new(b"not a zip".to_vec()), &d, 10).is_err());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn replace_moving_swaps_and_rolls_back() {
        let d = temp("moving");
        fs::write(d.join("a.exe"), b"old-a").unwrap();
        fs::write(d.join(".a.new"), b"new-a").unwrap();
        fs::write(d.join(".b.new"), b"new-b").unwrap();
        // b's staged file is missing: its rename fails and a is put back.
        let staged = vec![
            (d.join(".a.new"), d.join("a.exe")),
            (d.join(".missing"), d.join("b.exe")),
        ];
        assert!(replace_moving(&staged).is_err());
        assert_eq!(fs::read(d.join("a.exe")).unwrap(), b"old-a");
        assert!(!d.join("b.exe").exists());
        assert!(!d.join(".a.new").exists());

        fs::write(d.join(".a.new"), b"new-a").unwrap();
        let staged = vec![
            (d.join(".a.new"), d.join("a.exe")),
            (d.join(".b.new"), d.join("b.exe")),
        ];
        replace_moving(&staged).unwrap();
        assert_eq!(fs::read(d.join("a.exe")).unwrap(), b"new-a");
        assert_eq!(fs::read(d.join("b.exe")).unwrap(), b"new-b");
        let left: Vec<_> = fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(left.is_empty(), "{left:?}");
        let _ = fs::remove_dir_all(d);
    }
}
