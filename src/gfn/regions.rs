//! Manual server-selection data: PrintedWaste queue list, TCP ping, and ranking.
//!
//! Reference: `opennow-stable/src/main/services/printedWaste.ts` and `regionPing.ts`. The desktop
//! client pings every zone in the fleet and shows them all, grouped by region; the Vita keeps the
//! top `SHORTLIST_SIZE` of the best for the player's chosen `StreamRegion` (Auto = whole fleet).
//! Ranking is by measured ping alone - queue position only breaks ties or orders the zones whose
//! ping never came back - because the queue is a poor proxy for what is actually *best* for this
//! player.

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

const PRINTEDWASTE_QUEUE_URL: &str = "https://api.printedwaste.com/gfn/queue/";
/// Same 7 s ceiling the reference's `fetchWithTimeout` uses. The shared client's default is 15 s;
/// a slow PrintedWaste reply must not hold up a launch for that long.
const PRINTEDWASTE_TIMEOUT: Duration = Duration::from_secs(7);
/// One TCP connect may take the full 3 s on a flaky Wi-Fi link, the same ceiling as the
/// reference's `tcpPing`.
const PING_TIMEOUT: Duration = Duration::from_secs(3);
const PING_SAMPLE_GAP: Duration = Duration::from_millis(100);
/// The desktop averages three measured connects; two keep the pre-launch wait sane on the Vita,
/// and ping-only ranking means a single outlier barely moves the order.
const PING_MEASURED_SAMPLES: usize = 2;
/// How many of the best servers the shortlist keeps - a handful, not the whole fleet.
pub const SHORTLIST_SIZE: usize = 8;

/// Geographic filter for the server picker, persisted in settings. `Auto` pings the whole fleet
/// and shows the best overall; a chosen region only pings/ranks that region's zones. The variants
/// follow the PrintedWaste `Region` codes, with the two Southeast Asia codes under one option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamRegion {
    #[default]
    Auto,
    Us,
    Eu,
    Jp,
    Kr,
    Ca,
    In,
    /// PrintedWaste `THAI` and `MY`.
    Sea,
}

impl StreamRegion {
    pub const ALL: [StreamRegion; 8] = [
        Self::Auto,
        Self::Us,
        Self::Eu,
        Self::Jp,
        Self::Kr,
        Self::Ca,
        Self::In,
        Self::Sea,
    ];

    /// i18n key for the settings row label.
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Auto => "server-region-auto",
            Self::Us => "server-region-us",
            Self::Eu => "server-region-eu",
            Self::Jp => "server-region-jp",
            Self::Kr => "server-region-kr",
            Self::Ca => "server-region-ca",
            Self::In => "server-region-in",
            Self::Sea => "server-region-sea",
        }
    }

    /// Persisted text form in settings.json.
    pub fn as_text(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Us => "us",
            Self::Eu => "eu",
            Self::Jp => "jp",
            Self::Kr => "kr",
            Self::Ca => "ca",
            Self::In => "in",
            Self::Sea => "sea",
        }
    }

    pub fn from_text(text: &str) -> Self {
        match text.trim() {
            "us" => Self::Us,
            "eu" => Self::Eu,
            "jp" => Self::Jp,
            "kr" => Self::Kr,
            "ca" => Self::Ca,
            "in" => Self::In,
            "sea" => Self::Sea,
            _ => Self::Auto,
        }
    }

    /// Whether a PrintedWaste `Region` code (e.g. `EU`) belongs to this filter. `Auto` matches
    /// every zone.
    pub fn matches(self, printed_waste_region: &str) -> bool {
        match self {
            Self::Auto => true,
            Self::Us => printed_waste_region == "US",
            Self::Eu => printed_waste_region == "EU",
            Self::Jp => printed_waste_region == "JP",
            Self::Kr => printed_waste_region == "KR",
            Self::Ca => printed_waste_region == "CA",
            Self::In => printed_waste_region == "IN",
            Self::Sea => matches!(printed_waste_region, "THAI" | "MY"),
        }
    }
}

/// One candidate server/zone for the pre-launch picker.
#[derive(Debug, Clone)]
pub struct ServerCandidate {
    /// NVIDIA zone id, e.g. `NP-AMS-08` (kept as-sent; used for display and the base URL).
    pub zone_id: String,
    /// PrintedWaste region label, e.g. `EU`.
    pub region: String,
    /// Live queue position reported by PrintedWaste.
    pub queue_position: u32,
    /// Estimated wait in ms, when PrintedWaste sent one. Carried for the data model; the shortlist
    /// row shows queue + ping, not ETA.
    #[allow(dead_code)]
    pub eta_ms: Option<u32>,
    /// CloudMatch base for this zone: `https://{zone}.cloudmatchbeta.nvidiagrid.net/`.
    pub streaming_base_url: String,
    /// Measured TCP connect RTT in ms; `None` when every sample failed or timed out.
    pub ping_ms: Option<u32>,
}

/// True for the fleet zones the reference picker lists: `NP-*` standard zones, excluding the
/// `NPA-*` alliance zones.
pub fn is_standard_zone(zone_id: &str) -> bool {
    zone_id.starts_with("NP-") && !zone_id.starts_with("NPA-")
}

/// `NP-AMS-08` -> `https://np-ams-08.cloudmatchbeta.nvidiagrid.net/`, mirroring the reference's
/// `buildGfnZoneStreamingBaseUrl`.
fn streaming_base_url_for(zone_id: &str) -> String {
    format!(
        "https://{}.cloudmatchbeta.nvidiagrid.net/",
        zone_id.to_lowercase()
    )
}

/// Host portion of a streaming base URL, for the TCP ping.
fn zone_host(streaming_base_url: &str) -> &str {
    streaming_base_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .split('/')
        .next()
        .unwrap_or("")
}

#[derive(Debug, Deserialize)]
struct PrintedWasteEnvelope {
    status: bool,
    data: HashMap<String, serde_json::Value>,
}

/// Fetches the live queue list and builds a `ServerCandidate` per standard zone. Best-effort by
/// design: a zone missing `QueuePosition`/`Region` is skipped (mirroring the reference), but a
/// `status:false` reply or an empty result is an error so the caller falls back to default
/// routing instead of showing an empty picker.
pub async fn fetch_queues(client: &Client) -> Result<Vec<ServerCandidate>> {
    let response = client
        .get(PRINTEDWASTE_QUEUE_URL)
        .timeout(PRINTEDWASTE_TIMEOUT)
        .send()
        .await
        .context("PrintedWaste queue request failed")?;
    let payload: PrintedWasteEnvelope = response
        .json()
        .await
        .context("failed to decode PrintedWaste queue response")?;
    if !payload.status {
        bail!("PrintedWaste queue API returned status:false");
    }
    let candidates: Vec<ServerCandidate> = payload
        .data
        .iter()
        .filter_map(|(zone_id, value)| candidate_from_value(zone_id, value))
        .collect();
    if candidates.is_empty() {
        bail!("PrintedWaste queue API returned no standard zones");
    }
    Ok(candidates)
}

fn candidate_from_value(
    zone_id: &str,
    value: &serde_json::Value,
) -> Option<ServerCandidate> {
    if !is_standard_zone(zone_id) {
        return None;
    }
    let queue_position = value.get("QueuePosition")?.as_u64()? as u32;
    let region = value.get("Region")?.as_str()?;
    if region.is_empty() {
        return None;
    }
    Some(ServerCandidate {
        zone_id: zone_id.to_owned(),
        region: region.to_owned(),
        queue_position,
        eta_ms: value
            .get("eta")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        streaming_base_url: streaming_base_url_for(zone_id),
        ping_ms: None,
    })
}

/// Pings every candidate's zone host concurrently (one discarded warm-up + two measured connects)
/// and stamps the rounded average of the successful samples back onto each candidate.
pub async fn measure_pings(candidates: Vec<ServerCandidate>) -> Vec<ServerCandidate> {
    let handles: Vec<_> = candidates
        .into_iter()
        .map(|candidate| {
            let host = zone_host(&candidate.streaming_base_url).to_owned();
            tokio::spawn(async move {
                let mut candidate = candidate;
                candidate.ping_ms = measure_one(&host).await;
                candidate
            })
        })
        .collect();
    futures_util::future::join_all(handles)
        .await
        .into_iter()
        .filter_map(|result| result.ok())
        .collect()
}

async fn measure_one(host: &str) -> Option<u32> {
    // The first cold connect includes DNS resolution and TCP SYN overhead; discard it the way the
    // reference's `pingRegions` does before measuring.
    tcp_connect_rtt(host).await;
    let mut samples = Vec::with_capacity(PING_MEASURED_SAMPLES);
    for index in 0..PING_MEASURED_SAMPLES {
        if index > 0 {
            tokio::time::sleep(PING_SAMPLE_GAP).await;
        }
        if let Some(ms) = tcp_connect_rtt(host).await {
            samples.push(ms);
        }
    }
    if samples.is_empty() {
        return None;
    }
    let total: u64 = samples.iter().map(|&ms| u64::from(ms)).sum();
    Some((total as f64 / samples.len() as f64).round() as u32)
}

async fn tcp_connect_rtt(host: &str) -> Option<u32> {
    let started = std::time::Instant::now();
    tokio::time::timeout(PING_TIMEOUT, tokio::net::TcpStream::connect((host, 443)))
        .await
        .ok()?
        .ok()?;
    Some(started.elapsed().as_millis() as u32)
}

/// Sorts candidates by measured ping, lowest first. Zones without a measured ping always rank
/// below the measurable ones and sort by queue position, so the shortlist still prefers a short
/// wait when no latency is known. Queue position only breaks ping ties among measured zones - it
/// is never allowed to push a farther zone above a closer one.
pub fn rank(mut candidates: Vec<ServerCandidate>) -> Vec<ServerCandidate> {
    candidates.sort_by(|a, b| match (a.ping_ms, b.ping_ms) {
        (Some(a_ping), Some(b_ping)) => a_ping
            .cmp(&b_ping)
            .then_with(|| a.queue_position.cmp(&b.queue_position)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.queue_position.cmp(&b.queue_position),
    });
    candidates
}

/// Cuts a ranked list to the `n` best.
pub fn best_n(candidates: Vec<ServerCandidate>, n: usize) -> Vec<ServerCandidate> {
    candidates.into_iter().take(n).collect()
}

/// Fetch → filter to the chosen region → ping → rank → shortlist: the whole pipeline the launch
/// path spawns. A region that currently has no zones (PrintedWaste lists none under it) falls
/// back to the full fleet so the picker is never empty; `Auto` always means the full fleet.
pub async fn load_best_servers(
    client: &Client,
    region: StreamRegion,
) -> Result<Vec<ServerCandidate>> {
    let fetched = fetch_queues(client).await?;
    let filtered: Vec<ServerCandidate> = fetched
        .iter()
        .filter(|candidate| region.matches(&candidate.region))
        .cloned()
        .collect();
    let pool = if filtered.is_empty() {
        fetched
    } else {
        filtered
    };
    let measured = measure_pings(pool).await;
    Ok(best_n(rank(measured), SHORTLIST_SIZE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        zone_id: &str,
        region: &str,
        queue_position: u32,
        ping_ms: Option<u32>,
    ) -> ServerCandidate {
        ServerCandidate {
            zone_id: zone_id.to_owned(),
            region: region.to_owned(),
            queue_position,
            eta_ms: None,
            streaming_base_url: streaming_base_url_for(zone_id),
            ping_ms,
        }
    }

    #[test]
    fn standard_zone_filter_matches_the_reference() {
        assert!(is_standard_zone("NP-AMS-08"));
        assert!(is_standard_zone("NP-FRA-01"));
        // Alliance zones are not standard fleet.
        assert!(!is_standard_zone("NPA-LON-03"));
        // Anything else is not a zone.
        assert!(!is_standard_zone("prod"));
        assert!(!is_standard_zone(""));
    }

    #[test]
    fn zone_base_url_is_lowercased() {
        assert_eq!(
            streaming_base_url_for("NP-AMS-08"),
            "https://np-ams-08.cloudmatchbeta.nvidiagrid.net/"
        );
    }

    #[test]
    fn rank_puts_measured_ping_zones_first() {
        let mut ranked = rank(vec![
            candidate("NP-B", "EU", 5, None),
            candidate("NP-A", "EU", 50, Some(20)),
        ]);
        assert_eq!(ranked[0].zone_id, "NP-A");
        // unmeasured first element is now behind
        ranked.remove(0);
        assert_eq!(ranked[0].zone_id, "NP-B");
    }

    #[test]
    fn rank_prefers_lower_ping_when_queues_are_comparable() {
        let ranked = rank(vec![
            candidate("NP-SLOW", "EU", 1, Some(120)),
            candidate("NP-FAST", "EU", 3, Some(40)),
        ]);
        assert_eq!(ranked[0].zone_id, "NP-FAST");
    }

    #[test]
    fn rank_ignores_queue_for_measured_zones() {
        // Ping is the only ranking signal: a far-away-but-empty queue must never beat a closer
        // zone with a longer queue. NP-B has the best queue but the worst ping.
        let ranked = rank(vec![
            candidate("NP-FAR-EMPTY", "EU", 1, Some(250)),
            candidate("NP-NEAR-BUSY", "EU", 900, Some(12)),
        ]);
        assert_eq!(ranked[0].zone_id, "NP-NEAR-BUSY");
    }

    #[test]
    fn unmeasured_zones_sort_by_queue() {
        let ranked = rank(vec![
            candidate("NP-B", "EU", 10, None),
            candidate("NP-A", "EU", 2, None),
        ]);
        assert_eq!(ranked[0].zone_id, "NP-A");
    }

    #[test]
    fn stream_region_matches_its_codes() {
        assert!(StreamRegion::Auto.matches("US"));
        assert!(StreamRegion::Auto.matches("KR"));
        assert!(StreamRegion::Eu.matches("EU"));
        assert!(!StreamRegion::Eu.matches("US"));
        assert!(StreamRegion::Sea.matches("THAI"));
        assert!(StreamRegion::Sea.matches("MY"));
        assert!(!StreamRegion::Sea.matches("JP"));
        assert!(StreamRegion::In.matches("IN"));
        // Text round-trips so the persisted setting survives restarts.
        for region in StreamRegion::ALL {
            assert_eq!(StreamRegion::from_text(region.as_text()), region);
        }
        assert_eq!(StreamRegion::from_text("bogus"), StreamRegion::Auto);
    }

    #[test]
    fn best_n_caps_the_shortlist() {
        let many = (0..10)
            .map(|i| candidate(&format!("NP-{i:02}"), "EU", i as u32, Some(i as u32)))
            .collect();
        assert_eq!(best_n(many, SHORTLIST_SIZE).len(), SHORTLIST_SIZE);
        // The shortlist really is capped at eight, not at the old three.
        assert_eq!(SHORTLIST_SIZE, 8);
    }
}
