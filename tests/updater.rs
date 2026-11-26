//! Updater against a local HTTP server: release check (ETag, rate limit),
//! a full install into a temp install.sh layout, and the failure paths
//! (corrupt download, foreign redirect) that must leave it untouched.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferrochat::update::http::{self, CheckError, CheckOutcome};
use ferrochat::update::install::{self, InstallError, Stage};
use ferrochat::update::method::ScriptLayout;
use ferrochat::update::release::{Platform, select_package};
use ferrochat::update::{Version, release};

type Reply = (u16, Vec<(String, String)>, Vec<u8>);
type Routes = Arc<Mutex<HashMap<String, Reply>>>;

/// Minimal HTTP/1.1 server: one response per connection.
struct Server {
    base: String,
    routes: Routes,
    seen_etag: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes: Routes = Arc::default();
        let seen_etag: Arc<Mutex<Vec<String>>> = Arc::default();
        let (r, seen) = (routes.clone(), seen_etag.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let r = r.clone();
                let seen = seen.clone();
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() {
                        return;
                    }
                    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let mut inm = None;
                    loop {
                        let mut h = String::new();
                        if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
                            break;
                        }
                        if let Some((k, v)) = h.split_once(':')
                            && k.eq_ignore_ascii_case("if-none-match")
                        {
                            inm = Some(v.trim().to_string());
                        }
                    }
                    let reply = match (&inm, r.lock().unwrap().get(&path).cloned()) {
                        (Some(tag), Some((200, headers, _)))
                            if headers.iter().any(|(k, v)| k == "ETag" && v == tag) =>
                        {
                            seen.lock().unwrap().push(tag.clone());
                            (304, vec![], vec![])
                        }
                        (_, Some(reply)) => reply,
                        (_, None) => (404, vec![], b"not found".to_vec()),
                    };
                    let (status, headers, body) = reply;
                    let mut out = stream;
                    let mut head = format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                        body.len()
                    );
                    for (k, v) in headers {
                        head.push_str(&format!("{k}: {v}\r\n"));
                    }
                    head.push_str("\r\n");
                    let _ = out.write_all(head.as_bytes());
                    let _ = out.write_all(&body);
                });
            }
        });
        Self {
            base,
            routes,
            seen_etag,
        }
    }

    fn route(&self, path: &str, status: u16, headers: &[(&str, &str)], body: Vec<u8>) {
        self.routes.lock().unwrap().insert(
            path.into(),
            (
                status,
                headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                body,
            ),
        );
    }
}

const VERSION: &str = "9.0.0";

fn archive_name() -> String {
    format!("ferrochat-v{VERSION}-linux-x86_64.tar.gz")
}

/// A release tarball laid out like release.yml packages it.
fn tarball() -> Vec<u8> {
    let top = format!("ferrochat-v{VERSION}-linux-x86_64");
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut b = tar::Builder::new(gz);
    let mut add = |path: &str, mode: u32, data: &[u8]| {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(mode);
        h.set_cksum();
        b.append_data(&mut h, format!("{top}/{path}"), data)
            .unwrap();
    };
    add("ferrochat", 0o755, b"NEW-APP");
    add("ferrochat-player", 0o755, b"NEW-PLAYER");
    add("README.md", 0o644, b"readme");
    add(
        "packaging/dev.ferro.ferrochat.desktop",
        0o644,
        b"[Desktop Entry]\nName=Ferrochat\nExec=ferrochat %U\nIcon=dev.ferro.ferrochat\n",
    );
    add("packaging/dev.ferro.ferrochat.svg", 0o644, b"<svg/>");
    b.into_inner().unwrap().finish().unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "ferrochat-updater-it-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn release_json(base: &str) -> Vec<u8> {
    let name = archive_name();
    format!(
        r#"{{"tag_name":"v{VERSION}","draft":false,"prerelease":false,
        "html_url":"https://github.com/Kartudev/Ferrochat-kartu/releases/tag/v{VERSION}",
        "assets":[
          {{"name":"{name}","browser_download_url":"{base}/dl/{name}","size":0}},
          {{"name":"{name}.sha256","browser_download_url":"{base}/dl/{name}.sha256","size":0}}
        ]}}"#
    )
    .into_bytes()
}

/// Serves a release whose checksum file says `sum`.
fn serve_release(sum: &str) -> Server {
    let s = Server::start();
    let name = archive_name();
    s.route("/latest", 200, &[("ETag", "\"v9\"")], release_json(&s.base));
    s.route(&format!("/dl/{name}"), 200, &[], tarball());
    s.route(
        &format!("/dl/{name}.sha256"),
        200,
        &[],
        format!("{sum}  {name}\n").into_bytes(),
    );
    s
}

/// An install.sh layout with an old version installed.
fn old_install(name: &str) -> (PathBuf, ScriptLayout) {
    let root = temp(name);
    let layout = ScriptLayout {
        bin_dir: root.join("bin"),
        data_dir: root.join("share"),
        windows: false,
    };
    std::fs::create_dir_all(&layout.bin_dir).unwrap();
    std::fs::write(layout.bin("ferrochat"), b"OLD-APP").unwrap();
    std::fs::create_dir_all(layout.version_file().parent().unwrap()).unwrap();
    std::fs::write(layout.version_file(), "0.1.0\n").unwrap();
    (root, layout)
}

fn hidden_files(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
        .collect()
}

fn check(s: &Server, path: &str, etag: Option<&str>) -> Result<CheckOutcome, CheckError> {
    let (api, policy) = http::endpoint_for(&format!("{}{path}", s.base)).unwrap();
    http::fetch_latest(&api, &policy, etag)
}

#[test]
fn check_reads_the_release_and_honours_the_etag() {
    let s = serve_release(&"0".repeat(64));
    let CheckOutcome::Fresh { release, etag } = check(&s, "/latest", None).unwrap() else {
        panic!("expected a fresh answer");
    };
    let r = release.unwrap();
    assert_eq!(r.version, Version::new(9, 0, 0));
    assert_eq!(etag.as_deref(), Some("\"v9\""));
    assert!(select_package(&r, Platform::LinuxX86_64).is_some());
    assert_eq!(
        check(&s, "/latest", Some("\"v9\"")).unwrap(),
        CheckOutcome::NotModified
    );
    assert_eq!(s.seen_etag.lock().unwrap().as_slice(), ["\"v9\""]);
}

#[test]
fn rate_limits_and_errors() {
    let s = Server::start();
    s.route("/limited", 403, &[("Retry-After", "120")], vec![]);
    s.route("/busy", 429, &[], vec![]);
    s.route("/broken", 500, &[], vec![]);
    s.route("/junk", 200, &[], b"<html>".to_vec());
    assert_eq!(
        check(&s, "/limited", None),
        Err(CheckError::RateLimited {
            retry_after: Some(Duration::from_secs(120))
        })
    );
    assert_eq!(
        check(&s, "/busy", None),
        Err(CheckError::RateLimited { retry_after: None })
    );
    assert!(matches!(
        check(&s, "/broken", None),
        Err(CheckError::Failed(_))
    ));
    assert!(matches!(
        check(&s, "/junk", None),
        Err(CheckError::Failed(_))
    ));
    // No release published yet.
    assert_eq!(
        check(&s, "/missing", None),
        Ok(CheckOutcome::Fresh {
            release: None,
            etag: None
        })
    );
}

fn package_from(s: &Server) -> release::Package {
    let CheckOutcome::Fresh {
        release: Some(r), ..
    } = check(s, "/latest", None).unwrap()
    else {
        panic!("no release");
    };
    select_package(&r, Platform::LinuxX86_64).unwrap()
}

#[test]
fn installs_into_an_install_sh_layout() {
    let s = serve_release(&sha256_hex(&tarball()));
    let (root, layout) = old_install("ok");
    let (_, policy) = http::endpoint_for(&format!("{}/latest", s.base)).unwrap();
    let mut stages = Vec::new();
    install::install(
        &layout,
        &package_from(&s),
        &policy,
        &AtomicBool::new(false),
        &mut |st| stages.push(st),
    )
    .unwrap();
    assert_eq!(std::fs::read(layout.bin("ferrochat")).unwrap(), b"NEW-APP");
    assert_eq!(
        std::fs::read(layout.bin("ferrochat-player")).unwrap(),
        b"NEW-PLAYER"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(layout.bin("ferrochat"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
    assert_eq!(
        std::fs::read_to_string(layout.version_file()).unwrap(),
        "9.0.0\n"
    );
    let desktop =
        std::fs::read_to_string(layout.apps_dir().join("dev.ferro.ferrochat.desktop")).unwrap();
    assert!(desktop.contains(&format!("Exec={} %U\n", layout.bin("ferrochat").display())));
    assert!(layout.icon_dir().join("dev.ferro.ferrochat.svg").is_file());
    assert!(hidden_files(&layout.bin_dir).is_empty(), "temp files left");
    assert!(matches!(stages[0], Stage::Downloading { .. }));
    assert!(stages.contains(&Stage::Verifying));
    assert_eq!(stages.last(), Some(&Stage::Installing));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_corrupt_download_changes_nothing() {
    let s = serve_release(&"0".repeat(64));
    let (root, layout) = old_install("corrupt");
    let (_, policy) = http::endpoint_for(&format!("{}/latest", s.base)).unwrap();
    let r = install::install(
        &layout,
        &package_from(&s),
        &policy,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    assert!(matches!(r, Err(InstallError::Failed(m)) if m.contains("checksum")));
    assert_eq!(std::fs::read(layout.bin("ferrochat")).unwrap(), b"OLD-APP");
    assert!(!layout.bin("ferrochat-player").exists());
    assert_eq!(
        std::fs::read_to_string(layout.version_file()).unwrap(),
        "0.1.0\n"
    );
    assert!(hidden_files(&layout.bin_dir).is_empty(), "temp files left");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_cancelled_download_changes_nothing() {
    let s = serve_release(&sha256_hex(&tarball()));
    let (root, layout) = old_install("cancel");
    let (_, policy) = http::endpoint_for(&format!("{}/latest", s.base)).unwrap();
    let r = install::install(
        &layout,
        &package_from(&s),
        &policy,
        &AtomicBool::new(true),
        &mut |_| {},
    );
    assert_eq!(r, Err(InstallError::Cancelled));
    assert_eq!(std::fs::read(layout.bin("ferrochat")).unwrap(), b"OLD-APP");
    assert!(hidden_files(&layout.bin_dir).is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn redirects_are_checked_against_the_allowlist() {
    let s = Server::start();
    s.route("/ok", 200, &[], b"payload".to_vec());
    s.route("/hop", 302, &[("Location", "/ok")], vec![]);
    s.route(
        "/away",
        302,
        &[("Location", "http://evil.example/steal")],
        vec![],
    );
    let (_, policy) = http::endpoint_for(&format!("{}/latest", s.base)).unwrap();
    let get = |path: &str| {
        let mut out = Vec::new();
        http::download(
            &format!("{}{path}", s.base),
            &policy,
            &mut out,
            1024,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .map(|_| out)
    };
    assert_eq!(get("/hop").unwrap(), b"payload");
    assert!(matches!(get("/away"), Err(http::DownloadError::Failed(m)) if m.contains("refusing")));
    // Size cap.
    s.route("/big", 200, &[], vec![0u8; 4096]);
    assert!(get("/big").is_err());
    // An asset URL outside the allowlist is never requested.
    let mut out = Vec::new();
    assert!(
        http::download(
            "https://evil.example/x",
            &policy,
            &mut out,
            10,
            &AtomicBool::new(false),
            &mut |_, _| {}
        )
        .is_err()
    );
}

// --- Windows folder install (the release .zip) ------------------------------
// Pure file operations, so it runs on every platform; on Windows CI the old
// `ferrochat.exe` is a running process while it is replaced.

fn zip_name() -> String {
    format!("ferrochat-v{VERSION}-windows-x86_64.zip")
}

/// A release zip laid out like release.yml packages it.
fn release_zip() -> Vec<u8> {
    use std::io::Write as _;
    let top = format!("ferrochat-v{VERSION}-windows-x86_64");
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (path, data) in [
        ("ferrochat.exe", &b"NEW-APP"[..]),
        ("ferrochat-player.exe", b"NEW-PLAYER"),
        ("installed-version", b"9.0.0\n"),
        ("README.md", b"readme"),
        ("licenses/OpenH264-BSD-2-Clause.txt", b"bsd"),
    ] {
        z.start_file(format!("{top}/{path}"), opts).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap().into_inner()
}

fn serve_zip_release(sum: &str) -> Server {
    let s = Server::start();
    let name = zip_name();
    let base = s.base.clone();
    s.route(
        "/latest",
        200,
        &[],
        format!(
            r#"{{"tag_name":"v{VERSION}","draft":false,"prerelease":false,
            "html_url":"https://github.com/Kartudev/Ferrochat-kartu/releases/tag/v{VERSION}",
            "assets":[
              {{"name":"{name}","browser_download_url":"{base}/dl/{name}","size":0}},
              {{"name":"{name}.sha256","browser_download_url":"{base}/dl/{name}.sha256","size":0}}
            ]}}"#
        )
        .into_bytes(),
    );
    s.route(&format!("/dl/{name}"), 200, &[], release_zip());
    s.route(
        &format!("/dl/{name}.sha256"),
        200,
        &[],
        format!("{sum}  {name}\n").into_bytes(),
    );
    s
}

/// Keeps a copy of this test binary running from `exe` (it runs the ignored
/// `sleeper` test), like the app that installs its own update.
struct Running(std::process::Child);

impl Running {
    fn start(exe: &Path) -> Self {
        std::fs::copy(std::env::current_exe().unwrap(), exe).unwrap();
        let child = std::process::Command::new(exe)
            .args(["--ignored", "--exact", "sleeper", "--nocapture"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        Self(child)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "helper process for the Windows install test"]
fn sleeper() {
    std::thread::sleep(Duration::from_secs(60));
}

#[test]
fn installs_the_windows_zip_over_a_running_program() {
    let s = serve_zip_release(&sha256_hex(&release_zip()));
    let root = temp("win");
    let layout = ScriptLayout::windows(root.clone());
    let running = Running::start(&layout.bin("ferrochat"));
    std::fs::write(layout.bin("ferrochat-player"), b"OLD-PLAYER").unwrap();
    std::fs::write(layout.version_file(), "0.1.0\n").unwrap();

    let CheckOutcome::Fresh {
        release: Some(r), ..
    } = check(&s, "/latest", None).unwrap()
    else {
        panic!("no release");
    };
    let package = select_package(&r, Platform::WindowsX86_64).unwrap();
    let (_, policy) = http::endpoint_for(&format!("{}/latest", s.base)).unwrap();
    install::install(
        &layout,
        &package,
        &policy,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(std::fs::read(layout.bin("ferrochat")).unwrap(), b"NEW-APP");
    assert_eq!(
        std::fs::read(layout.bin("ferrochat-player")).unwrap(),
        b"NEW-PLAYER"
    );
    assert_eq!(
        std::fs::read_to_string(layout.version_file()).unwrap(),
        "9.0.0\n"
    );
    // The running program could only be moved aside (Windows); once it has
    // exited, the next start removes it.
    drop(running);
    install::clean_leftovers(&root);
    assert!(
        hidden_files(&root).is_empty(),
        "left: {:?}",
        hidden_files(&root)
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_corrupt_windows_zip_changes_nothing() {
    let s = serve_zip_release(&"0".repeat(64));
    let root = temp("win-corrupt");
    let layout = ScriptLayout::windows(root.clone());
    std::fs::write(layout.bin("ferrochat"), b"OLD-APP").unwrap();
    let CheckOutcome::Fresh {
        release: Some(r), ..
    } = check(&s, "/latest", None).unwrap()
    else {
        panic!("no release");
    };
    let package = select_package(&r, Platform::WindowsX86_64).unwrap();
    let (_, policy) = http::endpoint_for(&format!("{}/latest", s.base)).unwrap();
    let r = install::install(
        &layout,
        &package,
        &policy,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    assert!(matches!(r, Err(InstallError::Failed(m)) if m.contains("checksum")));
    assert_eq!(std::fs::read(layout.bin("ferrochat")).unwrap(), b"OLD-APP");
    assert!(hidden_files(&root).is_empty());
    let _ = std::fs::remove_dir_all(root);
}
