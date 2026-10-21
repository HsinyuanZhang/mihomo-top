//! A sanitized mihomo v1.19.32 `/connections` payload captured from the lab.

#[test]
fn v1_19_32_controller_frame_keeps_ports_as_strings_and_a_short_timestamp() {
    let value: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/v1.19.32-connections.json")).unwrap();
    mihomo_top::model::validate_envelope(&value).unwrap();
    let frame: mihomo_top::model::ConnectionsFrame = serde_json::from_value(value).unwrap();
    assert_eq!(frame.upload_total, 141);
    assert_eq!(frame.connections.len(), 2);

    let proxied = &frame.connections[0];
    assert_eq!(proxied.chain_display(), "mock-upstream");
    assert_eq!(proxied.display_host(), "127.0.0.1");
    assert_eq!(proxied.metadata.source_port, "48160");
    assert_eq!(proxied.source_socket().unwrap().port(), 48160);
    assert!(
        proxied.start_time().is_some(),
        "v1.19.32 start {} must parse",
        proxied.start
    );

    let direct = &frame.connections[1];
    assert_eq!(direct.chain_display(), "DIRECT");
    assert_eq!(direct.rule, "Match");
    assert!(direct.start_time().is_some());
}
