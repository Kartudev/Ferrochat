//! macOS: the tray is not implemented. The app reports "no tray",
//! so closing the window quits as before.

use super::TrayModel;

/// Always `false`: no tray on this platform yet.
pub async fn start(_initial: TrayModel) -> bool {
    false
}
