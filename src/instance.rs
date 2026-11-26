//! Single-instance hand-off. The OS file lock (`config::acquire_instance_lock`)
//! decides who is first; the first instance also listens on a Unix socket in
//! the (0700) data directory (on Windows: a loopback TCP port written to
//! `<data_dir>/ferrochat.port`). A second launch connects, sends `show`, and
//! exits: the first instance then shows and focuses its window, like the
//! official apps. The socket takes no other input.

use std::path::{Path, PathBuf};

/// `<data_dir>/ferrochat.sock`.
pub fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("ferrochat.sock")
}

/// The only message the socket understands.
pub const SHOW: &[u8] = b"show\n";

/// Whether a received line is the "show" request.
pub fn is_show(line: &[u8]) -> bool {
    line.trim_ascii() == b"show"
}

#[cfg(unix)]
pub use unix::{cleanup, notify_running, serve};

#[cfg(not(unix))]
pub use tcp::{cleanup, notify_running, serve};

/// `<data_dir>/ferrochat.port` (Windows): the loopback port of the first instance.
pub fn port_path(data_dir: &Path) -> PathBuf {
    data_dir.join("ferrochat.port")
}

/// Reads one short line from `stream` (anything longer or silent is dropped)
/// and asks the app to show its window if it is `show`.
fn handle(mut stream: impl std::io::Read) {
    let mut buf = [0u8; 16];
    let mut len = 0;
    while len < buf.len() {
        match stream.read(&mut buf[len..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                len += n;
                if buf[..len].contains(&b'\n') {
                    break;
                }
            }
        }
    }
    if is_show(&buf[..len]) {
        crate::ext::send(crate::ext::External::Show { chat: None });
    }
}

/// Loopback TCP hand-off for platforms without Unix sockets in std. Only
/// `show` is understood, so another local program can at most raise the
/// window.
#[cfg(not(unix))]
mod tcp {
    use std::io::Write;
    use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
    use std::path::Path;
    use std::time::Duration;

    use super::{SHOW, handle, port_path};

    /// Starts listening on an ephemeral loopback port and records it (call
    /// only while holding the instance lock). Runs on its own thread.
    pub fn serve(data_dir: &Path) -> std::io::Result<()> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        std::fs::write(port_path(data_dir), format!("{port}\n"))?;
        std::thread::Builder::new()
            .name("instance-socket".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                    handle(stream);
                }
            })?;
        Ok(())
    }

    /// Asks the running instance to show its window. Retries briefly: the
    /// first instance may still be writing its port file.
    pub fn notify_running(data_dir: &Path) -> std::io::Result<()> {
        let mut last = std::io::Error::other("no port file");
        for _ in 0..10 {
            let port = std::fs::read_to_string(port_path(data_dir))
                .ok()
                .and_then(|s| s.trim().parse::<u16>().ok());
            if let Some(port) = port {
                let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
                match TcpStream::connect_timeout(&addr, Duration::from_secs(1)) {
                    Ok(mut s) => {
                        s.set_write_timeout(Some(Duration::from_secs(1)))?;
                        return s.write_all(SHOW);
                    }
                    Err(e) => last = e,
                }
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        Err(last)
    }

    /// Removes the port file on exit.
    pub fn cleanup(data_dir: &Path) {
        let _ = std::fs::remove_file(port_path(data_dir));
    }
}

#[cfg(unix)]
mod unix {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::time::Duration;

    use super::{SHOW, handle, socket_path};

    /// Starts listening (call only while holding the instance lock, so any
    /// existing socket file is stale). Runs on its own thread.
    pub fn serve(data_dir: &Path) -> std::io::Result<()> {
        let path = socket_path(data_dir);
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        std::thread::Builder::new()
            .name("instance-socket".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                    handle(stream);
                }
            })?;
        Ok(())
    }

    /// Asks the running instance to show its window. Retries briefly: the
    /// first instance may still be binding its socket.
    pub fn notify_running(data_dir: &Path) -> std::io::Result<()> {
        let path = socket_path(data_dir);
        let mut last = std::io::Error::other("no socket");
        for _ in 0..10 {
            match UnixStream::connect(&path) {
                Ok(mut s) => {
                    s.set_write_timeout(Some(Duration::from_secs(1)))?;
                    return s.write_all(SHOW);
                }
                Err(e) => last = e,
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        Err(last)
    }

    /// Removes the socket file on exit.
    pub fn cleanup(data_dir: &Path) {
        let _ = std::fs::remove_file(socket_path(data_dir));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_line_parsing() {
        assert!(is_show(b"show\n"));
        assert!(is_show(b" show \r\n"));
        assert!(!is_show(b"quit\n"));
        assert!(!is_show(b""));
    }

    #[test]
    #[cfg(not(unix))]
    fn loopback_hand_off_reaches_the_first_instance() {
        let dir = std::env::temp_dir().join(format!("ferrochat-inst-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(notify_running(&dir).is_err(), "nobody listens yet");
        serve(&dir).unwrap();
        assert!(port_path(&dir).is_file());
        notify_running(&dir).unwrap();
        cleanup(&dir);
        assert!(!port_path(&dir).exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
