#[cfg(test)]
#[path = "../../host/src/request.rs"]
mod request;

#[cfg(test)]
mod tests {
    use super::request::{BODY_LIMIT, HEAD_LIMIT, decode_request};
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde_json::{Value, json};

    fn wire(fields: Value) -> String {
        let mut input =
            json!({"method":"POST","uri":"/bytes?","headers":[["x-test","one"],["x-test","two"]]});
        input
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        input.to_string()
    }

    #[test]
    fn canonical_and_legacy_bodies_preserve_bytes_and_head() {
        for size in [0, 1, 2, 3, 256, BODY_LIMIT - 2, BODY_LIMIT - 1, BODY_LIMIT] {
            let bytes: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            for fields in [
                json!({"body":bytes}),
                json!({"body_encoding":"base64-v1","body_base64":STANDARD.encode(&bytes)}),
            ] {
                let request = decode_request(&wire(fields)).unwrap();
                assert_eq!(request.body().as_ref(), bytes);
                assert_eq!(request.method(), "POST");
                assert_eq!(request.uri(), "/bytes?");
                assert_eq!(request.headers().get_all("x-test").iter().count(), 2);
            }
        }
    }

    #[test]
    fn multibyte_body_is_bytes_without_text_normalization() {
        let bytes = "你好 🌍 café\0".as_bytes();
        for fields in [
            json!({"body":bytes}),
            json!({"body_encoding":"base64-v1","body_base64":STANDARD.encode(bytes)}),
        ] {
            assert_eq!(
                decode_request(&wire(fields)).unwrap().body().as_ref(),
                bytes
            );
        }
    }

    #[test]
    fn invalid_request_heads_are_rejected_for_both_encodings() {
        for body in [
            json!({"body":[]}),
            json!({"body_encoding":"base64-v1","body_base64":""}),
        ] {
            for invalid in [
                json!({"method":"bad method"}),
                json!({"uri":"/bad uri"}),
                json!({"headers":[["bad name","value"]]}),
                json!({"headers":[["x-test","bad\r\nvalue"]]}),
            ] {
                let mut input: Value = serde_json::from_str(&wire(body.clone())).unwrap();
                input
                    .as_object_mut()
                    .unwrap()
                    .extend(invalid.as_object().unwrap().clone());
                assert!(decode_request(&input.to_string()).is_err());
            }
        }
    }

    #[test]
    fn rejects_noncanonical_base64() {
        for encoded in [
            "AB==", "AAB=", "AA", "AAA", "AA-_", "AA==\n", "AA ==", "====", "A===", "AA=A",
            "AA==AAAA", "éAAA",
        ] {
            assert!(
                decode_request(&wire(
                    json!({"body_encoding":"base64-v1","body_base64":encoded})
                ))
                .is_err(),
                "{encoded:?}"
            );
        }
    }

    #[test]
    fn rejects_missing_ambiguous_null_or_unknown_fields() {
        for fields in [
            json!({}),
            json!({"body_base64":""}),
            json!({"body_encoding":"base64-v1"}),
            json!({"body_encoding":"base64-v2","body_base64":""}),
            json!({"body":[],"body_encoding":"base64-v1","body_base64":""}),
            json!({"body":null,"body_encoding":"base64-v1","body_base64":""}),
            json!({"body":[],"body_encoding":null}),
            json!({"body":[],"body_base64":null}),
            json!({"body_encoding":"base64-v1","body_base64":null}),
            json!({"body_encoding":null,"body_base64":""}),
            json!({"body":null}),
            json!({"body":[],"extra":true}),
            json!({"body_encoding":"base64-v1","body_base64":[]}),
            json!({"body":[256]}),
            json!({"body":[-1]}),
            json!({"body":[1.5]}),
        ] {
            assert!(decode_request(&wire(fields.clone())).is_err(), "{fields}");
        }
        for fields in [
            r#""body":[],"body":[]"#,
            r#""body_encoding":"base64-v1","body_base64":"","body_base64":"""#,
            r#""body_encoding":"base64-v1","body_encoding":"base64-v1","body_base64":"""#,
        ] {
            assert!(
                decode_request(&format!(
                    r#"{{"method":"POST","uri":"/bytes","headers":[],{fields}}}"#
                ))
                .is_err()
            );
        }
    }

    #[test]
    fn bounds_decoded_bytes_even_when_encoded_length_fits() {
        // 65536, 65537 and 65538 share the same padded encoded length.
        for size in [BODY_LIMIT + 1, BODY_LIMIT + 2] {
            let encoded = STANDARD.encode(vec![255; size]);
            assert_eq!(encoded.len(), 4 * BODY_LIMIT.div_ceil(3));
            let error = decode_request(&wire(
                json!({"body_encoding":"base64-v1","body_base64":encoded}),
            ))
            .unwrap_err();
            assert_eq!(error, "request body exceeds bound");
        }
    }

    #[test]
    fn bounds_encoded_allocation_and_legacy_body() {
        let encoded = "A".repeat(4 * BODY_LIMIT.div_ceil(3) + 4);
        assert_eq!(
            decode_request(&wire(
                json!({"body_encoding":"base64-v1","body_base64":encoded})
            ))
            .unwrap_err(),
            "encoded request body exceeds bound"
        );
        assert_eq!(
            decode_request(&wire(json!({"body":vec![0; BODY_LIMIT + 1]}))).unwrap_err(),
            "request body exceeds bound"
        );
        assert_eq!(
            decode_request(&" ".repeat(BODY_LIMIT * 4 + HEAD_LIMIT * 6 + 1)).unwrap_err(),
            "serialized request exceeds bound"
        );
    }
}
