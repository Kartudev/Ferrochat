//! App-level updater flows: banner, dismissal, settings check, mock mode.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use super::Message;
use super::test_support::Harness;
use crate::update::http::{CheckOutcome, endpoint_for};
use crate::update::release::{Release, tag_page};
use crate::update::{Banner, CheckState, UpdateMsg, Version};

fn newer() -> Version {
    let c = Version::current();
    Version::new(c.major, c.minor + 1, 0)
}

fn release(v: Version) -> Release {
    Release {
        version: v,
        html_url: tag_page(v),
        assets: vec![],
    }
}

async fn send(h: &mut Harness, m: UpdateMsg) {
    let t = h.app.update(Message::Update(m));
    h.pump(t).await;
}

/// Serves one `releases/latest` answer.
fn serve_once(body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/latest", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let mut r = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut l = String::new();
                if r.read_line(&mut l).is_err() || l.trim().is_empty() {
                    break;
                }
            }
            let mut s = stream;
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"t\"\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    url
}

#[tokio::test]
async fn nothing_runs_in_mock_mode() {
    let mut h = Harness::new().await;
    assert!(!h.app.updater.enabled());
    assert_eq!(h.app.updater.check, CheckState::Disabled);
    assert!(h.app.updater.path.is_none(), "no disk writes in mock mode");
    assert_eq!(h.app.updates_boot().units(), 0);
    send(&mut h, UpdateMsg::CheckNow).await;
    assert_eq!(h.app.updater.check, CheckState::Disabled);
    let g = h.app.updater.generation;
    send(&mut h, UpdateMsg::Due(g)).await;
    assert_eq!(h.app.updater.check, CheckState::Disabled);
    assert!(h.app.updater.banner().is_none());
    assert!(crate::ui::update::banner(&h.app).is_none());
}

#[tokio::test]
async fn banner_appears_and_dismissal_persists() {
    let mut h = Harness::new().await;
    let dir = std::env::temp_dir().join(format!("ferrochat-upd-flow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("update.json");
    h.app.updater.path = Some(path.clone());
    send(
        &mut h,
        UpdateMsg::Checked {
            manual: false,
            result: Ok(CheckOutcome::Fresh {
                release: Some(release(newer())),
                etag: None,
            }),
        },
    )
    .await;
    assert_eq!(h.app.updater.banner(), Some(Banner::Available(newer())));
    // The whole view builds with the banner on top.
    let _ = crate::ui::view(&h.app);
    assert!(crate::ui::update::banner(&h.app).is_some());

    send(&mut h, UpdateMsg::Dismiss).await;
    assert_eq!(h.app.updater.banner(), None);
    let saved = crate::update::state::Persisted::load(&path);
    assert_eq!(saved.dismissed, Some(newer()));
    assert_eq!(saved.release.map(|r| r.version), Some(newer()));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn settings_check_finds_an_update() {
    let mut h = Harness::new().await;
    let v = newer();
    let url = serve_once(format!(
        r#"{{"tag_name":"v{v}","html_url":"https://github.com/Kartudev/Ferrochat-kartu/releases/tag/v{v}","assets":[]}}"#
    ));
    h.app.updater.api = endpoint_for(&url);
    h.app.updater.check = CheckState::Idle;
    h.app.settings_open = true;
    send(&mut h, UpdateMsg::CheckNow).await;
    assert_eq!(h.app.updater.available().map(|r| r.version), Some(v));
    assert_eq!(h.app.updater.check, CheckState::Idle);
    assert!(crate::ui::update::check_label(&h.app).contains("is available"));
    // The settings page with the section builds.
    let _ = crate::ui::view(&h.app);
}

#[tokio::test]
async fn auto_check_toggle() {
    let mut h = Harness::new().await;
    assert!(h.app.settings.check_updates);
    let g = h.app.updater.generation;
    send(&mut h, UpdateMsg::ToggleAuto).await;
    assert!(!h.app.settings.check_updates);
    assert_ne!(h.app.updater.generation, g, "the old timer chain is stale");
    send(&mut h, UpdateMsg::ToggleAuto).await;
    assert!(h.app.settings.check_updates);
}
