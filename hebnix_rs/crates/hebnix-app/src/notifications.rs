//! One-shot startup announcements. Network/JSON failures are deliberately silent.

use std::time::Duration;

use serde::Deserialize;

const ENDPOINT: &str = "https://api.hebnix.com/notifications";

#[derive(Debug, Clone, Deserialize)]
pub struct Notification {
    pub id: u64,
    pub message: String,
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default, rename = "link-text")]
    pub link_text: Option<String>,
}

impl Notification {
    pub fn is_newer_than(&self, dismissed_id: u64) -> bool {
        self.id > dismissed_id && !self.message.trim().is_empty()
    }

    pub fn link_url(&self) -> Option<reqwest::Url> {
        let link = self.link.as_deref()?.trim();
        if link.is_empty() {
            return None;
        }
        let url = reqwest::Url::parse(ENDPOINT).ok()?.join(link).ok()?;
        matches!(url.scheme(), "http" | "https").then_some(url)
    }
}

pub fn check_on_start(
    dismissed_id: u64,
    tx: crossbeam_channel::Sender<crate::messages::AppMsg>,
    ctx: eframe::egui::Context,
) {
    let _ = std::thread::Builder::new()
        .name("notification-checker".into())
        .spawn(move || {
            let notification = (|| {
                ureq::AgentBuilder::new()
                    .try_proxy_from_env(false)
                    .build()
                    .get(ENDPOINT)
                    .set("User-Agent", "Hebnix")
                    .timeout(Duration::from_secs(10))
                    .call()
                    .ok()?
                    .into_json::<Notification>()
                    .ok()
            })();
            if let Some(notification) = notification.filter(|n| n.is_newer_than(dismissed_id)) {
                let _ = tx.send(crate::messages::AppMsg::NotificationFetched(notification));
                ctx.request_repaint();
            }
        });
}
