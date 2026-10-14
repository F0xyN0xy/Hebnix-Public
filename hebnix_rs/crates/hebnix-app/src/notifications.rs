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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_new_ids_with_a_message_are_shown() {
        let notification: Notification =
            serde_json::from_str(r#"{"id":4,"message":"Hello"}"#).unwrap();
        assert!(notification.is_newer_than(0));
        assert!(notification.is_newer_than(3));
        assert!(!notification.is_newer_than(4));
        assert!(!notification.is_newer_than(5));
        assert!(notification.link_url().is_none());
        assert!(serde_json::from_str::<Notification>(r#"{"message":"Hello"}"#).is_err());
        assert!(serde_json::from_str::<Notification>(r#"{"id":true,"message":"Hello"}"#).is_err());
        assert!(
            !Notification {
                message: "  ".into(),
                ..notification
            }
            .is_newer_than(0)
        );
    }

    #[test]
    fn joins_relative_links_and_preserves_absolute_web_links() {
        let mut notification: Notification = serde_json::from_str(
            r#"{"id":1,"message":"Hello","link":"/news","link-text":"Read more"}"#,
        )
        .unwrap();
        assert_eq!(notification.link_text.as_deref(), Some("Read more"));
        assert_eq!(
            notification.link_url().unwrap().as_str(),
            "https://api.hebnix.com/news"
        );
        notification.link = Some("https://hebnix.com/news".into());
        assert_eq!(
            notification.link_url().unwrap().as_str(),
            "https://hebnix.com/news"
        );
        notification.link = Some("javascript:alert(1)".into());
        assert!(notification.link_url().is_none());
    }

    #[test]
    fn dismissed_id_survives_config_round_trip_and_defaults_for_old_configs() {
        let mut config: crate::config::Config = toml::from_str("").unwrap();
        assert_eq!(config.notification_id, 0);
        config.notification_id = 42;
        let saved = toml::to_string(&config).unwrap();
        let loaded: crate::config::Config = toml::from_str(&saved).unwrap();
        assert_eq!(loaded.notification_id, 42);
    }
}
