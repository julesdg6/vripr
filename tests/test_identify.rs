use vripr::metadata::identify::{
    duration_agreement, fingerprint_segment, parse_fpcalc_output, rank_score,
};

#[test]
fn parses_fpcalc_json() {
    let fingerprint = parse_fpcalc_output(br#"{"duration":123.4,"fingerprint":"AQADt..."}"#).unwrap();
    assert_eq!(fingerprint.duration, 123.4);
    assert_eq!(fingerprint.fingerprint, "AQADt...");
}

#[test]
fn rejects_invalid_fpcalc_output() {
    assert!(parse_fpcalc_output(br#"{"duration":0,"fingerprint":""}"#).is_err());
}

#[test]
fn reports_missing_fpcalc_actionably() {
    let error = fingerprint_segment(
        "/definitely/not/fpcalc", std::path::Path::new("/tmp/audio.flac"), 0.0, 60.0,
    ).unwrap_err();
    assert!(error.to_string().contains("Install Chromaprint"));
}

#[test]
fn ranking_rewards_fingerprint_and_duration() {
    assert!(rank_score(0.9, 1.0, false) > rank_score(0.9, 0.0, false));
    assert!(rank_score(0.9, 1.0, true) > rank_score(0.9, 1.0, false));
    assert_eq!(duration_agreement(180.0, Some(180.0)), 1.0);
    assert_eq!(duration_agreement(180.0, Some(220.0)), 0.0);
}
