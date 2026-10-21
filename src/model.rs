//! Serde model for the mihomo `GET /connections` frame (WebSocket push).
//!
//! Field names follow the live v1.19.25 payload (camelCase). Unknown fields
//! are ignored by serde (forward compatibility for schema *additions*).
//! The core always serializes the `connections` key (a nil Go slice becomes
//! JSON `null`), so a missing key means we are not looking at a connections
//! frame at all — `validate_envelope` rejects that instead of silently
//! presenting zero-values as facts.

use std::net::{IpAddr, SocketAddr};

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// `null` (the core's encoding of an empty connection table) deserializes
/// as an empty vec instead of failing the whole frame.
fn null_to_empty_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    let opt: Option<Vec<T>> = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

/// Reject payloads that lack the envelope key entirely. A missing
/// `connections` field is not an empty table — it is not a connections
/// frame.
pub fn validate_envelope(value: &serde_json::Value) -> Result<(), String> {
    let Some(obj) = value.as_object() else {
        return Err("payload is not a JSON object".to_string());
    };
    match obj.get("connections") {
        Some(serde_json::Value::Array(_)) | Some(serde_json::Value::Null) => Ok(()),
        Some(other) => Err(format!(
            "connections has unexpected type: {}",
            type_name(other)
        )),
        None => Err("connections key missing (not a connections frame?)".to_string()),
    }
}

fn type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionsFrame {
    pub upload_total: u64,
    pub download_total: u64,
    #[serde(deserialize_with = "null_to_empty_vec")]
    pub connections: Vec<ConnectionEntry>,
    /// wall-clock time at which we received and parsed this frame
    #[serde(skip)]
    pub received_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionEntry {
    pub id: String,
    pub metadata: Metadata,
    pub upload: u64,
    pub download: u64,
    /// RFC3339 start time, e.g. "2026-10-21T01:23:45.678901234+08:00"
    #[serde(default)]
    pub start: String,
    /// Outbound chain as reported by the core. Observed order on v1.19.25 is
    /// `[exit_node, group, ...]`, i.e. `chains[0]` is the concrete exit.
    #[serde(default)]
    pub chains: Vec<String>,
    #[serde(default)]
    pub rule: String,
    #[serde(default)]
    pub rule_payload: String,
}

impl ConnectionEntry {
    pub fn start_time(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.start)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    }

    /// The local socket this connection entered the core through — the join
    /// key into `/proc/net/{tcp,udp}{,6}`. IPv4-mapped IPv6 addresses are
    /// canonicalized so both join sides compare equal.
    pub fn source_socket(&self) -> Option<SocketAddr> {
        Some(SocketAddr::new(
            self.metadata
                .source_ip
                .parse::<IpAddr>()
                .ok()?
                .to_canonical(),
            self.metadata.source_port.parse::<u16>().ok()?,
        ))
    }

    /// Display host: SNI/CONNECT host when present, else destination IP.
    pub fn display_host(&self) -> &str {
        if self.metadata.host.is_empty() {
            &self.metadata.destination_ip
        } else {
            &self.metadata.host
        }
    }

    /// Human-readable outbound path, rendered local → exit.
    pub fn chain_display(&self) -> String {
        match self.chains.len() {
            0 => String::new(),
            1 => self.chains[0].clone(),
            _ => self
                .chains
                .iter()
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join("→"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    #[serde(default)]
    pub network: String,
    #[serde(default)]
    pub r#type: String,
    /// The live payload spells this "sourceIP" (not camelCase "sourceIp").
    #[serde(rename = "sourceIP")]
    pub source_ip: String,
    #[serde(default, rename = "destinationIP")]
    pub destination_ip: String,
    #[serde(default)]
    pub source_port: String,
    #[serde(default)]
    pub destination_port: String,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub process: String,
    #[serde(default)]
    pub process_path: String,
    #[serde(default)]
    pub remote_destination: String,
    #[serde(default)]
    pub dns_mode: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Representative v1.19.25 controller shape with synthetic values.
    const LIVE_FRAME: &str = r#"{
      "downloadTotal": 5267570269,
      "uploadTotal": 12029892290,
      "connections": [
        {
          "id": "8f2a-1",
          "metadata": {
            "network": "tcp",
            "type": "Mixed",
            "sourceIP": "127.0.0.1",
            "destinationIP": "",
            "sourcePort": "47226",
            "destinationPort": "443",
            "host": "api.example.test",
            "process": "",
            "processPath": "",
            "remoteDestination": "",
            "dnsMode": "normal"
          },
          "upload": 12345,
          "download": 67890,
          "start": "2026-10-21T01:23:45.678901234+08:00",
          "chains": ["Node", "Proxy"],
          "rule": "Match",
          "rulePayload": ""
        },
        {
          "id": "fixture-udp",
          "metadata": {"network": "udp", "sourceIP": "127.0.0.1",
                       "sourcePort": "40001", "destinationPort": "53",
                       "host": "", "destinationIP": "203.0.113.9"},
          "upload": 0,
          "download": 0,
          "chains": ["DIRECT"],
          "rule": "_dns"
        }
      ]
    }"#;

    #[test]
    fn deserializes_live_shaped_frame() {
        let f: ConnectionsFrame = serde_json::from_str(LIVE_FRAME).unwrap();
        assert_eq!(f.upload_total, 12_029_892_290);
        assert_eq!(f.download_total, 5_267_570_269);
        assert_eq!(f.connections.len(), 2);

        let c = &f.connections[0];
        assert_eq!(c.display_host(), "api.example.test");
        assert_eq!(c.chain_display(), "Proxy→Node");
        assert_eq!(c.rule, "Match");
        assert_eq!(c.metadata.source_port, "47226");
        let sock = c.source_socket().unwrap();
        assert_eq!(sock.ip().to_string(), "127.0.0.1");
        assert_eq!(sock.port(), 47226);
        assert!(c.start_time().is_some());
        assert_eq!(c.upload, 12345);
        assert_eq!(c.download, 67890);
    }

    #[test]
    fn null_connections_is_a_legal_empty_table() {
        // The core serializes a nil slice as null.
        let f: ConnectionsFrame =
            serde_json::from_str(r#"{"uploadTotal":0,"downloadTotal":0,"connections":null}"#)
                .unwrap();
        assert!(f.connections.is_empty());
        assert!(validate_envelope(
            &serde_json::from_str::<serde_json::Value>(
                r#"{"uploadTotal":0,"downloadTotal":0,"connections":null}"#
            )
            .unwrap()
        )
        .is_ok());
    }

    #[test]
    fn empty_array_connections_is_legal() {
        let f: ConnectionsFrame =
            serde_json::from_str(r#"{"uploadTotal":5,"downloadTotal":0,"connections":[]}"#)
                .unwrap();
        assert!(f.connections.is_empty());
        assert_eq!(f.upload_total, 5);
    }

    #[test]
    fn envelope_rejects_missing_connections_key() {
        // `{}` must not be accepted as a zero-value fact.
        for bad in ["{}", r#"{"uploadTotal":123}"#, r#"{"x":1}"#, "[]"] {
            let v: serde_json::Value = serde_json::from_str(bad).unwrap();
            assert!(validate_envelope(&v).is_err(), "should reject {bad}");
        }
    }

    #[test]
    fn envelope_rejects_wrong_typed_connections() {
        let v: serde_json::Value = serde_json::from_str(r#"{"connections":42}"#).unwrap();
        assert!(validate_envelope(&v).is_err());
    }

    #[test]
    fn mapped_ipv6_source_canonicalizes_to_v4() {
        // An AF_INET6 client to the mixed port reports ::ffff:127.0.0.1
        // in /proc but 127.0.0.1 in mihomo metadata; both must join
        let f: ConnectionsFrame = serde_json::from_str(
            r#"{"uploadTotal":0,"downloadTotal":0,"connections":[{"id":"mapped","metadata":{"sourceIP":"::ffff:127.0.0.1","sourcePort":"40002"},"upload":0,"download":0}]}"#,
        )
        .unwrap();
        assert_eq!(
            f.connections[0].source_socket().unwrap().ip().to_string(),
            "127.0.0.1"
        );
    }

    #[test]
    fn empty_host_falls_back_to_destination_ip() {
        let f: ConnectionsFrame = serde_json::from_str(LIVE_FRAME).unwrap();
        let c = &f.connections[1];
        assert_eq!(c.display_host(), "203.0.113.9");
        assert_eq!(c.chain_display(), "DIRECT");
    }

    #[test]
    fn default_entry_is_inert() {
        let c = ConnectionEntry::default();
        assert_eq!(c.chain_display(), "");
        assert!(c.start_time().is_none());
        assert!(c.source_socket().is_none());
        assert_eq!(c.display_host(), "");
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let f: ConnectionsFrame = serde_json::from_str(
            r#"{"uploadTotal":0,"downloadTotal":0,"connections":[{"id":"future","metadata":{"sourceIP":"127.0.0.1","brandNewField":true},"upload":0,"download":0}],"futureField":1}"#,
        )
        .unwrap();
        assert_eq!(f.connections.len(), 1);
    }

    #[test]
    fn serde_rejects_missing_required_envelope_and_connection_facts() {
        for bad in [
            r#"{"downloadTotal":0,"connections":[]}"#,
            r#"{"uploadTotal":0,"connections":[]}"#,
            r#"{"uploadTotal":0,"downloadTotal":0}"#,
            r#"{"uploadTotal":0,"downloadTotal":0,"connections":[{"metadata":{},"upload":0,"download":0}]}"#,
            r#"{"uploadTotal":0,"downloadTotal":0,"connections":[{"id":"x","upload":0,"download":0}]}"#,
            r#"{"uploadTotal":0,"downloadTotal":0,"connections":[{"id":"x","metadata":{},"download":0}]}"#,
        ] {
            assert!(
                serde_json::from_str::<ConnectionsFrame>(bad).is_err(),
                "accepted {bad}"
            );
        }
    }
}
