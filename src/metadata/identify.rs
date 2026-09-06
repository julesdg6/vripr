use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

const ACOUSTID_URL: &str = "https://api.acoustid.org/v2/lookup";
const MUSICBRAINZ_URL: &str = "https://musicbrainz.org/ws/2/recording";

#[derive(Debug, Clone, PartialEq)]
pub struct Fingerprint {
    pub duration: f64,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IdentificationCandidate {
    pub recording_id: String,
    pub artist: String,
    pub title: String,
    pub release: String,
    pub fingerprint_score: f32,
    pub duration_score: f32,
    pub score: f32,
}

#[derive(Deserialize)]
struct FpcalcOutput {
    duration: f64,
    fingerprint: String,
}

#[derive(Deserialize)]
struct AcoustIdResponse {
    status: String,
    #[serde(default)]
    results: Vec<AcoustIdResult>,
}

#[derive(Deserialize)]
struct AcoustIdResult {
    score: f32,
    #[serde(default)]
    recordings: Vec<AcoustIdRecording>,
}

#[derive(Deserialize)]
struct AcoustIdRecording {
    id: String,
}

#[derive(Deserialize)]
struct MusicBrainzRecording {
    title: String,
    length: Option<i64>,
    #[serde(default)]
    #[serde(rename = "artist-credit")]
    artist_credit: Vec<ArtistCredit>,
    #[serde(default)]
    releases: Vec<MusicBrainzRelease>,
}

#[derive(Deserialize)]
struct ArtistCredit {
    name: String,
}

#[derive(Deserialize)]
struct MusicBrainzRelease {
    title: String,
}

/// Run fpcalc for a bounded segment. Its JSON output is parsed without ever
/// logging the fingerprint, which is an identifier derived from the audio.
pub fn fingerprint_segment(
    fpcalc_path: &str,
    audio_path: &Path,
    start_secs: f64,
    length_secs: f64,
) -> Result<Fingerprint> {
    let executable = if fpcalc_path.trim().is_empty() { "fpcalc" } else { fpcalc_path };
    let offset = start_secs.max(0.0).floor().to_string();
    let length = length_secs.clamp(1.0, 120.0).floor().to_string();
    let output = Command::new(executable)
        .args([
            "-json",
            "-offset", &offset,
            "-length", &length,
        ])
        .arg(audio_path)
        .output()
        .with_context(|| format!(
            "Could not run fpcalc ({executable}). Install Chromaprint/fpcalc or set its path in Settings"
        ))?;
    if !output.status.success() {
        bail!(
            "fpcalc failed. Check that it can read the analysis audio and that its configured path is correct"
        );
    }
    parse_fpcalc_output(&output.stdout)
}

pub fn parse_fpcalc_output(output: &[u8]) -> Result<Fingerprint> {
    let fingerprint: FpcalcOutput = serde_json::from_slice(output)
        .context("fpcalc returned unreadable JSON")?;
    if fingerprint.fingerprint.is_empty() || fingerprint.duration <= 0.0 {
        bail!("fpcalc did not return a usable fingerprint");
    }
    Ok(Fingerprint { duration: fingerprint.duration, fingerprint: fingerprint.fingerprint })
}

pub fn duration_agreement(observed: f64, candidate: Option<f64>) -> f32 {
    let Some(candidate) = candidate.filter(|d| *d > 0.0) else { return 0.5 };
    (1.0 - ((observed - candidate).abs() / 30.0) as f32).clamp(0.0, 1.0)
}

pub fn rank_score(fingerprint_score: f32, duration_score: f32, hint_match: bool) -> f32 {
    (fingerprint_score.clamp(0.0, 1.0) * 0.75
        + duration_score.clamp(0.0, 1.0) * 0.20
        + if hint_match { 0.05 } else { 0.0 })
        .clamp(0.0, 1.0)
}

pub async fn identify(
    client: &reqwest::Client,
    acoustid_key: &str,
    fingerprint: &Fingerprint,
    track_duration: f64,
    title_hint: &str,
    artist_hint: &str,
) -> Result<Vec<IdentificationCandidate>> {
    if acoustid_key.trim().is_empty() {
        bail!("AcoustID API key is not set — add it in Settings → API Keys");
    }
    let duration = fingerprint.duration.round().to_string();
    let response = client.get(ACOUSTID_URL)
        .query(&[
            ("client", acoustid_key),
            ("meta", "recordings"),
            ("duration", &duration),
            ("fingerprint", &fingerprint.fingerprint),
        ])
        .send().await.map_err(|_| anyhow::anyhow!("AcoustID request failed"))?;
    if !response.status().is_success() {
        bail!("AcoustID request was rejected");
    }
    let response: AcoustIdResponse = response.json().await
        .map_err(|_| anyhow::anyhow!("AcoustID returned an unreadable response"))?;
    if response.status != "ok" {
        bail!("AcoustID could not process the lookup");
    }

    let mut candidates = Vec::new();
    for result in response.results.into_iter().take(5) {
        for recording in result.recordings.into_iter().take(3) {
            let recording_data: MusicBrainzRecording = client
                .get(format!("{MUSICBRAINZ_URL}/{}", recording.id))
                .query(&[("inc", "artists+releases"), ("fmt", "json")])
                .send().await.context("MusicBrainz request failed")?
                .error_for_status().context("MusicBrainz request was rejected")?
                .json().await.context("MusicBrainz returned an unreadable response")?;
            let artist = recording_data.artist_credit.iter()
                .map(|credit| credit.name.as_str()).collect::<Vec<_>>().join(", ");
            let release = recording_data.releases.first()
                .map(|release| release.title.clone()).unwrap_or_default();
            let hint = !title_hint.is_empty() && recording_data.title.eq_ignore_ascii_case(title_hint)
                || !artist_hint.is_empty() && artist.eq_ignore_ascii_case(artist_hint);
            let duration_score = duration_agreement(
                track_duration,
                recording_data.length.map(|milliseconds| milliseconds as f64 / 1000.0),
            );
            candidates.push(IdentificationCandidate {
                recording_id: recording.id,
                artist,
                title: recording_data.title,
                release,
                fingerprint_score: result.score,
                duration_score,
                score: rank_score(result.score, duration_score, hint),
            });
        }
    }
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    candidates.dedup_by(|a, b| a.recording_id == b.recording_id);
    Ok(candidates)
}
