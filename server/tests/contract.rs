//! Wire-contract tests: the server must accept exactly the payloads the client
//! emits. These deserialize the shared fixtures in tracker/test/fixtures/wire/
//! (the same files the client's contract.test.ts asserts its builders produce),
//! so a field rename or type change on either side of the TS<->Rust boundary
//! breaks CI instead of silently dropping data in production.

use dullahan::types::RawPayload;

macro_rules! fixture {
    ($path:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tracker/test/fixtures/wire/",
            $path
        ))
    };
}

fn parse(json: &str) -> RawPayload {
    serde_json::from_str(json).expect("fixture should deserialize into RawPayload")
}

#[test]
fn pageview_fixture_round_trips() {
    let mut p = parse(fixture!("pageview.json"));
    assert!(matches!(p, RawPayload::Pageview { .. }));
    assert_eq!(p.site_id(), "demo");
    p.validate().expect("valid pageview");
}

#[test]
fn event_fixture_round_trips() {
    let mut p = parse(fixture!("event.json"));
    assert!(matches!(p, RawPayload::Event { .. }));
    p.validate().expect("valid event");
}

#[test]
fn performance_fixture_round_trips() {
    let mut p = parse(fixture!("performance.json"));
    assert!(matches!(p, RawPayload::Performance { .. }));
    p.validate().expect("valid performance");
}

#[test]
fn pageleave_fixture_round_trips() {
    let mut p = parse(fixture!("pageleave.json"));
    assert!(matches!(p, RawPayload::Pageleave { .. }));
    p.validate().expect("valid pageleave");
}

#[test]
fn unknown_type_is_rejected_at_deserialize() {
    assert!(serde_json::from_str::<RawPayload>(fixture!("invalid/unknown-type.json")).is_err());
}

#[test]
fn missing_event_name_is_rejected_at_deserialize() {
    assert!(
        serde_json::from_str::<RawPayload>(fixture!("invalid/missing-event-name.json")).is_err()
    );
}

#[test]
fn empty_site_is_rejected_by_validate() {
    // Deserializes fine (s is just a string) but validate() must reject it.
    let mut p = parse(fixture!("invalid/empty-site.json"));
    assert!(p.validate().is_err());
}
