//! Port of packages/protocol/test/protocol.test.ts

use indexmap::IndexMap;
use pi_js::json;
use pi_js::testing::assert_json_matches;
use pi_protocol::protocol::ServerTarget;
use pi_protocol::{
    AttachmentEnvelope, CancelEnvelope, CborValue, ClientHello, ClientMessage, ClientMessageDecoder, FrameDecoder,
    FrameDecoderOptions, PROTOCOL_VERSION, ProtocolError, ProtocolValidationError, RequestEnvelope, ResponseEnvelope,
    RpcTarget, ServerHello, ServerMessage, ServerMessageDecoder, ServiceEventEnvelope, SessionTarget, decode_cbor,
    encode_cbor, encode_client_message, encode_frame, encode_server_message, is_server_id,
    is_supported_protocol_version, parse_client_message, parse_server_message,
};
use serde_json::{Value, json as j};

const VALID_SERVER_ID: &str = "00000000-0000-4000-8000-000000000001";

fn client_hello_msg() -> ClientMessage {
    ClientMessage::Hello(ClientHello {
        version: PROTOCOL_VERSION,
    })
}

fn server_hello_msg() -> ServerMessage {
    ServerMessage::Hello(ServerHello {
        version: PROTOCOL_VERSION,
        server_id: VALID_SERVER_ID.to_string(),
    })
}

fn cbor_map<I, K>(entries: I) -> CborValue
where
    I: IntoIterator<Item = (K, CborValue)>,
    K: Into<String>,
{
    let mut map = IndexMap::new();
    for (k, v) in entries {
        map.insert(k.into(), v);
    }
    CborValue::Map(map)
}

fn assert_validation_error(err: pi_js::Error) -> String {
    assert!(
        err.downcast_ref::<ProtocolValidationError>().is_some(),
        "expected ProtocolValidationError, got {err:?}"
    );
    err.message()
}

mod protocol_validation {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn negotiates_protocol_version_8() {
        assert_eq!(PROTOCOL_VERSION, 8);
        assert!(is_supported_protocol_version(8.0));
        assert!(!is_supported_protocol_version(7.0));
        assert!(!is_supported_protocol_version(8.5));
    }

    #[test]
    fn accepts_integer_client_hello_version_for_negotiation() {
        for version in [0, PROTOCOL_VERSION, PROTOCOL_VERSION + 1] {
            let input = cbor_map([
                ("type", CborValue::String("hello".to_string())),
                ("version", CborValue::Number(version as f64)),
            ]);
            assert_eq!(
                parse_client_message(&input).unwrap(),
                ClientMessage::Hello(ClientHello { version })
            );
        }
    }

    #[test]
    fn rejects_an_invalid_client_hello() {
        let cases = [
            cbor_map([
                ("type", CborValue::String("hello".to_string())),
                ("version", CborValue::String(PROTOCOL_VERSION.to_string())),
            ]),
            cbor_map([
                ("type", CborValue::String("hello".to_string())),
                ("version", CborValue::Number(PROTOCOL_VERSION as f64 + 0.5)),
            ]),
            cbor_map([
                ("type", CborValue::String("hello".to_string())),
                ("version", CborValue::Number(PROTOCOL_VERSION as f64)),
                ("extra", CborValue::Bool(true)),
            ]),
        ];
        for message in cases {
            let err = parse_client_message(&message).expect_err("invalid client hello");
            assert_validation_error(err);
        }
    }

    #[test]
    fn rejects_non_canonical_uuidv4_server_id() {
        assert!(is_server_id(&Value::String(VALID_SERVER_ID.to_string())));
        for server_id in [
            "",
            "server-1",
            "00000000-0000-7000-8000-000000000001",
            "00000000-0000-4000-7000-000000000001",
            "00000000-0000-4000-8000-00000000000A",
        ] {
            assert!(!is_server_id(&Value::String(server_id.to_string())), "{server_id:?}");
            let message = cbor_map([
                ("type", CborValue::String("request".to_string())),
                ("id", CborValue::String("request-1".to_string())),
                (
                    "target",
                    cbor_map([("serverId", CborValue::String(server_id.to_string()))]),
                ),
                (
                    "call",
                    cbor_map([
                        ("serviceId", CborValue::String("pi.models".to_string())),
                        ("member", CborValue::String("list".to_string())),
                        ("args", CborValue::Array(vec![])),
                    ]),
                ),
            ]);
            let err = parse_client_message(&message).expect_err(server_id);
            assert_validation_error(err);
        }
    }

    #[test]
    fn keeps_routed_request_and_event_payloads_opaque() {
        let request = ClientMessage::Request(RequestEnvelope {
            id: "request-1".to_string(),
            target: RpcTarget::Session(SessionTarget {
                server_id: VALID_SERVER_ID.to_string(),
                session_id: "session-1".to_string(),
                attachment_id: "attachment-1".to_string(),
            }),
            call: j!({
                "serviceId": "application.custom",
                "instance": { "key": "instance-1", "generation": 2 },
                "member": "invoke",
                "args": [{ "arbitrary": true }, ["opaque"]],
            }),
        });
        let request_cbor = cbor_map([
            ("type", CborValue::String("request".to_string())),
            ("id", CborValue::String("request-1".to_string())),
            (
                "target",
                cbor_map([
                    ("serverId", CborValue::String(VALID_SERVER_ID.to_string())),
                    ("sessionId", CborValue::String("session-1".to_string())),
                    ("attachmentId", CborValue::String("attachment-1".to_string())),
                ]),
            ),
            (
                "call",
                cbor_map([
                    ("serviceId", CborValue::String("application.custom".to_string())),
                    (
                        "instance",
                        cbor_map([
                            ("key", CborValue::String("instance-1".to_string())),
                            ("generation", CborValue::Number(2.0)),
                        ]),
                    ),
                    ("member", CborValue::String("invoke".to_string())),
                    (
                        "args",
                        CborValue::Array(vec![
                            cbor_map([("arbitrary", CborValue::Bool(true))]),
                            CborValue::Array(vec![CborValue::String("opaque".to_string())]),
                        ]),
                    ),
                ]),
            ),
        ]);
        assert_eq!(parse_client_message(&request_cbor).unwrap(), request);

        let arbitrary_call = cbor_map([
            ("type", CborValue::String("request".to_string())),
            ("id", CborValue::String("request-1".to_string())),
            (
                "target",
                cbor_map([
                    ("serverId", CborValue::String(VALID_SERVER_ID.to_string())),
                    ("sessionId", CborValue::String("session-1".to_string())),
                    ("attachmentId", CborValue::String("attachment-1".to_string())),
                ]),
            ),
            (
                "call",
                cbor_map([(
                    "arbitrary",
                    CborValue::String("strict JSON whose service meaning belongs to Chord".to_string()),
                )]),
            ),
        ]);
        let parsed_request = parse_client_message(&arbitrary_call).unwrap();
        let parsed_request_json = serde_json::to_value(&parsed_request).unwrap();
        assert!(
            parsed_request_json
                .get("call")
                .and_then(|c| c.get("arbitrary"))
                .is_some_and(Value::is_string)
        );

        let update_cbor = cbor_map([
            ("type", CborValue::String("service_update".to_string())),
            ("subscriptionId", CborValue::String("subscription-1".to_string())),
            ("update", cbor_map([("applicationDefined", CborValue::Bool(true))])),
        ]);
        let parsed_update = parse_server_message(&update_cbor).unwrap();
        let parsed_update_json = serde_json::to_value(&parsed_update).unwrap();
        assert_json_matches(&parsed_update_json, &j!({ "update": { "applicationDefined": true } }));
    }

    #[test]
    fn rejects_non_json_opaque_payloads() {
        // PORT: `undefined` properties and cyclic object references cannot be expressed in `CborValue`.
        for (label, value) in [
            ("byte array", CborValue::Bytes(vec![1])),
            ("non-finite number", CborValue::Number(f64::NAN)),
        ] {
            let req = cbor_map([
                ("type", CborValue::String("request".to_string())),
                ("id", CborValue::String("request-1".to_string())),
                (
                    "target",
                    cbor_map([("serverId", CborValue::String(VALID_SERVER_ID.to_string()))]),
                ),
                (
                    "call",
                    cbor_map([
                        ("serviceId", CborValue::String("application.custom".to_string())),
                        ("member", CborValue::String("invoke".to_string())),
                        ("args", CborValue::Array(vec![value.clone()])),
                    ]),
                ),
            ]);
            assert_validation_error(parse_client_message(&req).expect_err(label));

            let resp = cbor_map([
                ("type", CborValue::String("response".to_string())),
                ("id", CborValue::String("request-1".to_string())),
                ("ok", CborValue::Bool(true)),
                ("result", value),
            ]);
            assert_validation_error(parse_server_message(&resp).expect_err(label));
        }
    }

    #[test]
    fn validates_request_cancellation_envelopes() {
        let cancel = ClientMessage::Cancel(CancelEnvelope {
            id: "request-1".to_string(),
            target: RpcTarget::Server(ServerTarget {
                server_id: VALID_SERVER_ID.to_string(),
            }),
        });
        let cancel_cbor = cbor_map([
            ("type", CborValue::String("cancel".to_string())),
            ("id", CborValue::String("request-1".to_string())),
            (
                "target",
                cbor_map([("serverId", CborValue::String(VALID_SERVER_ID.to_string()))]),
            ),
        ]);
        assert_eq!(parse_client_message(&cancel_cbor).unwrap(), cancel);

        let empty_id = cbor_map([
            ("type", CborValue::String("cancel".to_string())),
            ("id", CborValue::String(String::new())),
            (
                "target",
                cbor_map([("serverId", CborValue::String(VALID_SERVER_ID.to_string()))]),
            ),
        ]);
        assert_validation_error(parse_client_message(&empty_id).expect_err("empty id"));

        let extra_field = cbor_map([
            ("type", CborValue::String("cancel".to_string())),
            ("id", CborValue::String("request-1".to_string())),
            (
                "target",
                cbor_map([("serverId", CborValue::String(VALID_SERVER_ID.to_string()))]),
            ),
            ("extra", CborValue::Bool(true)),
        ]);
        assert_validation_error(parse_client_message(&extra_field).expect_err("extra field"));
    }

    #[test]
    fn validates_attachment_route_updates() {
        let attached = ServerMessage::Attachment(AttachmentEnvelope {
            attachment: Some(SessionTarget {
                server_id: VALID_SERVER_ID.to_string(),
                session_id: "session-1".to_string(),
                attachment_id: "attachment-1".to_string(),
            }),
        });
        let attached_cbor = cbor_map([
            ("type", CborValue::String("attachment".to_string())),
            (
                "attachment",
                cbor_map([
                    ("serverId", CborValue::String(VALID_SERVER_ID.to_string())),
                    ("sessionId", CborValue::String("session-1".to_string())),
                    ("attachmentId", CborValue::String("attachment-1".to_string())),
                ]),
            ),
        ]);
        assert_eq!(parse_server_message(&attached_cbor).unwrap(), attached);

        let detached = ServerMessage::Attachment(AttachmentEnvelope { attachment: None });
        let detached_cbor = cbor_map([
            ("type", CborValue::String("attachment".to_string())),
            ("attachment", CborValue::Null),
        ]);
        assert_eq!(parse_server_message(&detached_cbor).unwrap(), detached);

        let invalid_attached = cbor_map([
            ("type", CborValue::String("attachment".to_string())),
            (
                "attachment",
                cbor_map([("sessionId", CborValue::String("session-1".to_string()))]),
            ),
        ]);
        assert_validation_error(parse_server_message(&invalid_attached).expect_err("invalid attachment"));
    }

    #[test]
    fn rejects_malformed_request_boundaries() {
        let cases = [
            (
                "empty request id",
                cbor_map([
                    ("type", CborValue::String("request".to_string())),
                    ("id", CborValue::String(String::new())),
                    (
                        "target",
                        cbor_map([("serverId", CborValue::String(VALID_SERVER_ID.to_string()))]),
                    ),
                    (
                        "call",
                        cbor_map([
                            ("serviceId", CborValue::String("pi.models".to_string())),
                            ("member", CborValue::String("list".to_string())),
                            ("args", CborValue::Array(vec![])),
                        ]),
                    ),
                ]),
            ),
            (
                "extra envelope field",
                cbor_map([
                    ("type", CborValue::String("request".to_string())),
                    ("id", CborValue::String("request-1".to_string())),
                    (
                        "target",
                        cbor_map([("serverId", CborValue::String(VALID_SERVER_ID.to_string()))]),
                    ),
                    (
                        "call",
                        cbor_map([
                            ("serviceId", CborValue::String("pi.models".to_string())),
                            ("member", CborValue::String("list".to_string())),
                            ("args", CborValue::Array(vec![])),
                        ]),
                    ),
                    ("extra", CborValue::Bool(true)),
                ]),
            ),
        ];
        for (label, message) in cases {
            assert_validation_error(parse_client_message(&message).expect_err(label));
        }
    }

    #[test]
    fn accepts_a_successful_void_response_without_a_result_field() {
        let void_cbor = cbor_map([
            ("type", CborValue::String("response".to_string())),
            ("id", CborValue::String("request-1".to_string())),
            ("ok", CborValue::Bool(true)),
        ]);
        assert_eq!(
            parse_server_message(&void_cbor).unwrap(),
            ServerMessage::Response(ResponseEnvelope {
                id: "request-1".to_string(),
                ok: true,
                result: None,
                error: None,
            })
        );

        let null_result_cbor = cbor_map([
            ("type", CborValue::String("response".to_string())),
            ("id", CborValue::String("request-1".to_string())),
            ("ok", CborValue::Bool(true)),
            ("result", CborValue::Null),
        ]);
        assert_eq!(
            parse_server_message(&null_result_cbor).unwrap(),
            ServerMessage::Response(ResponseEnvelope {
                id: "request-1".to_string(),
                ok: true,
                result: Some(Value::Null),
                error: None,
            })
        );
    }

    #[test]
    fn rejects_malformed_server_boundaries() {
        let cases = [
            (
                "invalid server id",
                cbor_map([
                    ("type", CborValue::String("hello".to_string())),
                    ("version", CborValue::Number(PROTOCOL_VERSION as f64)),
                    ("serverId", CborValue::String("server-1".to_string())),
                ]),
            ),
            (
                "extra response field",
                cbor_map([
                    ("type", CborValue::String("response".to_string())),
                    ("id", CborValue::String("request-1".to_string())),
                    ("ok", CborValue::Bool(true)),
                    ("result", CborValue::Array(vec![])),
                    ("extra", CborValue::Bool(true)),
                ]),
            ),
            (
                "empty error code",
                cbor_map([
                    ("type", CborValue::String("response".to_string())),
                    ("id", CborValue::String("request-1".to_string())),
                    ("ok", CborValue::Bool(false)),
                    (
                        "error",
                        cbor_map([
                            ("code", CborValue::String(String::new())),
                            ("message", CborValue::String("bad".to_string())),
                        ]),
                    ),
                ]),
            ),
        ];
        for (label, message) in cases {
            assert_validation_error(parse_server_message(&message).expect_err(label));
        }
    }

    #[test]
    fn accepts_the_opaque_error_code() {
        for code in ["wrong_server", "cancelled", "service_not_found", "application_error"] {
            let expected = ServerMessage::Response(ResponseEnvelope {
                id: "request-1".to_string(),
                ok: false,
                result: None,
                error: Some(ProtocolError {
                    code: code.to_string(),
                    message: "safe".to_string(),
                }),
            });
            let cbor = cbor_map([
                ("type", CborValue::String("response".to_string())),
                ("id", CborValue::String("request-1".to_string())),
                ("ok", CborValue::Bool(false)),
                (
                    "error",
                    cbor_map([
                        ("code", CborValue::String(code.to_string())),
                        ("message", CborValue::String("safe".to_string())),
                    ]),
                ),
            ]);
            assert_eq!(parse_server_message(&cbor).unwrap(), expected);
        }
    }

    #[test]
    fn rejects_unknown_messages_and_fields() {
        let extra_hello = cbor_map([
            ("type", CborValue::String("hello".to_string())),
            ("version", CborValue::Number(PROTOCOL_VERSION as f64)),
            ("serverId", CborValue::String(VALID_SERVER_ID.to_string())),
            ("snapshot", cbor_map([] as [(&str, CborValue); 0])),
        ]);
        assert_validation_error(parse_server_message(&extra_hello).expect_err("extra snapshot"));

        let unknown_msg = cbor_map([
            ("type", CborValue::String("unknown".to_string())),
            ("event", cbor_map([] as [(&str, CborValue); 0])),
        ]);
        assert_validation_error(parse_server_message(&unknown_msg).expect_err("unknown type"));
    }

    #[test]
    fn does_not_parse_json_strings_as_messages() {
        let client_str = CborValue::String(json::stringify(&client_hello_msg()));
        assert_validation_error(parse_client_message(&client_str).expect_err("client string"));
        let server_str = CborValue::String(json::stringify(&server_hello_msg()));
        assert_validation_error(parse_server_message(&server_str).expect_err("server string"));
    }
}

mod validated_framed_protocol_apis {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn encodes_complete_client_and_server_frames() {
        let client_hello = client_hello_msg();
        let client_frames = FrameDecoder::new(None)
            .push(&encode_client_message(&client_hello, None).unwrap())
            .unwrap();
        assert_eq!(
            parse_client_message(&decode_cbor(&client_frames[0], None).unwrap()).unwrap(),
            client_hello
        );

        let server_hello = server_hello_msg();
        let server_frames = FrameDecoder::new(None)
            .push(&encode_server_message(&server_hello, None).unwrap())
            .unwrap();
        assert_eq!(
            parse_server_message(&decode_cbor(&server_frames[0], None).unwrap()).unwrap(),
            server_hello
        );
    }

    #[test]
    fn enforces_outbound_frame_limits() {
        let opts = Some(FrameDecoderOptions {
            max_frame_length: Some(8),
        });
        assert_validation_error(encode_client_message(&client_hello_msg(), opts).expect_err("client limit"));
        assert_validation_error(encode_server_message(&server_hello_msg(), opts).expect_err("server limit"));
    }

    #[test]
    fn incrementally_decodes_fragmented_and_coalesced_client_messages() {
        let client_hello = client_hello_msg();
        let request = ClientMessage::Request(RequestEnvelope {
            id: "request-1".to_string(),
            target: RpcTarget::Server(ServerTarget {
                server_id: VALID_SERVER_ID.to_string(),
            }),
            call: j!({ "serviceId": "pi.session-directory", "member": "list", "args": [] }),
        });
        let first = encode_client_message(&client_hello, None).unwrap();
        let second = encode_client_message(&request, None).unwrap();
        let mut wire = Vec::with_capacity(first.len() + second.len());
        wire.extend_from_slice(&first);
        wire.extend_from_slice(&second);

        for split in 0..=wire.len() {
            let decoder = ClientMessageDecoder::new(None);
            let mut messages = Vec::new();
            messages.extend(decoder.push(&wire[..split]).unwrap());
            messages.extend(decoder.push(&wire[split..]).unwrap());
            decoder.end().unwrap();
            assert_eq!(messages, vec![client_hello.clone(), request.clone()]);
        }
    }

    #[test]
    fn incrementally_decodes_fragmented_and_coalesced_server_messages() {
        let server_hello = server_hello_msg();
        let response = ServerMessage::Response(ResponseEnvelope {
            id: "request-1".to_string(),
            ok: true,
            result: Some(j!([])),
            error: None,
        });
        let first = encode_server_message(&server_hello, None).unwrap();
        let second = encode_server_message(&response, None).unwrap();
        let mut wire = Vec::with_capacity(first.len() + second.len());
        wire.extend_from_slice(&first);
        wire.extend_from_slice(&second);

        let split = first.len() + second.len() / 2;
        let decoder = ServerMessageDecoder::new(None);
        assert_eq!(decoder.push(&wire[..split]).unwrap(), vec![server_hello]);
        assert_eq!(decoder.push(&wire[split..]).unwrap(), vec![response]);
        decoder.end().unwrap();
    }

    #[test]
    fn rejects_invalid_framed_input() {
        let invalid_schema_cbor = encode_cbor(
            &cbor_map([
                ("type", CborValue::String("hello".to_string())),
                ("version", CborValue::Number(1.0)),
                ("extra", CborValue::Bool(true)),
            ]),
            None,
        )
        .unwrap();
        let cases = [
            ("empty CBOR payload", encode_frame(&[]).unwrap()),
            ("malformed CBOR", encode_frame(&[0xff]).unwrap()),
            ("schema-invalid CBOR", encode_frame(&invalid_schema_cbor).unwrap()),
        ];
        for (label, wire) in cases {
            let decoder = ClientMessageDecoder::new(None);
            assert_validation_error(decoder.push(&wire).expect_err(label));
            let msg = assert_validation_error(
                decoder
                    .push(&encode_client_message(&client_hello_msg(), None).unwrap())
                    .expect_err("push after failed"),
            );
            assert!(msg.to_lowercase().contains("failed"), "unexpected error: {msg}");
        }
    }

    #[test]
    fn rejects_truncated_and_oversized_framing() {
        let truncated = ServerMessageDecoder::new(None);
        assert_eq!(truncated.push(&[0, 0, 0, 2, 1]).unwrap(), vec![]);
        assert_validation_error(truncated.end().expect_err("truncated end"));

        let oversized = ClientMessageDecoder::new(Some(FrameDecoderOptions {
            max_frame_length: Some(3),
        }));
        assert_validation_error(oversized.push(&[0, 0, 0, 4]).expect_err("oversized header"));
    }

    #[test]
    fn handles_service_event_and_neg_zero_hello_version() {
        let event = ServerMessage::ServiceUpdate(ServiceEventEnvelope {
            subscription_id: "sub-1".to_string(),
            update: j!({ "delta": 1 }),
        });
        let encoded = encode_server_message(&event, None).unwrap();
        let dec = ServerMessageDecoder::new(None);
        assert_eq!(dec.push(&encoded).unwrap(), vec![event]);
        dec.end().unwrap();

        let neg_zero_hello = cbor_map([
            ("type", CborValue::String("hello".to_string())),
            ("version", CborValue::Number(-0.0)),
        ]);
        assert_eq!(
            parse_client_message(&neg_zero_hello).unwrap(),
            ClientMessage::Hello(ClientHello { version: 0 })
        );
    }
}
