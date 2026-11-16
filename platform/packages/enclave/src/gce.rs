//! What the machine's own metadata server says (TRUST.md#keep-it-alive): its
//! name and zone, and a maintenance notice, which the runner reports in its
//! heartbeat so the keeper starts a second machine to take the BundleKey
//! before this one goes. Only in an image; nothing here is trusted beyond
//! that.

use std::time::Duration;

const BASE: &str = "http://metadata.google.internal/computeMetadata/v1/instance";

fn get(path: &str) -> Option<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(2)))
        .build()
        .into();
    agent
        .get(format!("{BASE}/{path}"))
        .header("Metadata-Flavor", "Google")
        .call()
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()
        .map(|s| s.trim().to_string())
}

/// This machine's name and zone, if it's on GCP.
pub fn identity() -> Option<(String, String)> {
    let name = get("name")?;
    let zone = get("zone")?.rsplit('/').next()?.to_string();
    Some((name, zone))
}

/// Whether maintenance is coming for this machine: GCP's scheduled notice
/// (`upcoming-maintenance`, days ahead for a TDX machine, which can't
/// live-migrate: TRUST.md#keeping-the-bundlekey-alive), or the imminent
/// event, about a minute before a stop.
pub fn maintenance_noticed() -> Option<String> {
    noticed(
        get("upcoming-maintenance").as_deref(),
        get("maintenance-event").as_deref(),
    )
}

/// The notice, if any, as the heartbeat reports it (at most 200 bytes, so a
/// mismatch with GCP's own view can be read). A scheduled notice counts only
/// as a JSON object naming its window or status: something else came back
/// on the testnet where GCP had nothing scheduled. `NONE` is no imminent
/// event.
fn noticed(upcoming: Option<&str>, event: Option<&str>) -> Option<String> {
    let scheduled = upcoming
        .and_then(|u| serde_json::from_str::<serde_json::Value>(u).ok())
        .filter(|v| {
            v["window_start_time"].is_string()
                || matches!(
                    v["maintenance_status"].as_str(),
                    Some("PENDING" | "ONGOING")
                )
        });
    if let Some(v) = scheduled {
        return Some(v.to_string().chars().take(200).collect());
    }
    event
        .filter(|e| *e != "NONE")
        .map(|e| e.chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_scheduled_notice_counts_days_ahead_not_only_the_last_minute() {
        let scheduled = r#"{"maintenance_status":"PENDING","type":"SCHEDULED","window_start_time":"2026-11-18T02:00:00Z"}"#;
        assert!(super::noticed(Some(scheduled), Some("NONE")).is_some());
        assert!(super::noticed(None, Some("TERMINATE_ON_HOST_MAINTENANCE")).is_some());
        assert!(super::noticed(None, Some("NONE")).is_none());
        assert!(super::noticed(Some(""), None).is_none());
        // Anything else at that key isn't a notice.
        assert!(super::noticed(Some("not found"), Some("NONE")).is_none());
        assert!(super::noticed(Some("{}"), Some("NONE")).is_none());
    }
}
