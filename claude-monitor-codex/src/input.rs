//! Validate browser-owned inline raster bytes before building native UserInput items.
use super::{RouteError, RouteResult};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};

const IMAGE_BYTES: usize = 4 * 1024 * 1024;
const TOTAL_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const IMAGE_COUNT: usize = 4;

/// Text-only callers remain valid; an image-only message may omit or empty its text.
/// Accept only bounded inline images, never remote URLs or caller filesystem paths.
pub(super) fn message(payload: &Value) -> RouteResult {
    let text = match payload.get("text") {
        None => "",
        Some(Value::String(text)) if text.len() <= 128 * 1024 && !text.contains('\0') => text,
        _ => {
            return Err(RouteError::Invalid(
                "text must be a string of at most 131072 bytes without NUL".to_owned(),
            ))
        }
    };
    let mut input = Vec::new();
    if !text.trim().is_empty() {
        input.push(json!({"type":"text","text":text,"textElements":[]}));
    }
    let mut total = 0;
    if let Some(images) = payload.get("images") {
        let images = images
            .as_array()
            .filter(|images| images.len() <= IMAGE_COUNT)
            .ok_or_else(|| {
                RouteError::Invalid(
                    "images must be an array of at most 4 inline image URLs".to_owned(),
                )
            })?;
        for (index, image) in images.iter().enumerate() {
            let url = image.as_str().ok_or_else(|| {
                RouteError::Invalid("Each image must be an inline image URL".to_owned())
            })?;
            let (header, encoded) = url.split_once(',').ok_or_else(|| {
                RouteError::Invalid("Images must use data:image/...;base64 URLs".to_owned())
            })?;
            if ![
                "data:image/png;base64",
                "data:image/jpeg;base64",
                "data:image/webp;base64",
                "data:image/gif;base64",
            ]
            .contains(&header)
            {
                return Err(RouteError::Invalid(
                    "Only inline PNG, JPEG, WebP and GIF images are supported".to_owned(),
                ));
            }
            if encoded.len() > IMAGE_BYTES.div_ceil(3) * 4 {
                return Err(RouteError::Invalid(
                    "Each image must be at most 4 MiB".to_owned(),
                ));
            }
            let bytes = STANDARD.decode(encoded).map_err(|error| {
                RouteError::Invalid(format!(
                    "Image {} contains invalid base64: {error}",
                    index + 1
                ))
            })?;
            if bytes.is_empty() || bytes.len() > IMAGE_BYTES {
                return Err(RouteError::Invalid(
                    "Each image must contain between 1 byte and 4 MiB".to_owned(),
                ));
            }
            total += bytes.len();
            if total > TOTAL_IMAGE_BYTES {
                return Err(RouteError::Invalid(
                    "Images must total at most 8 MiB".to_owned(),
                ));
            }
            let matches = match header {
                "data:image/png;base64" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
                "data:image/jpeg;base64" => bytes.starts_with(&[255, 216, 255]),
                "data:image/gif;base64" => {
                    bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")
                }
                "data:image/webp;base64" => {
                    bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP"
                }
                _ => false,
            };
            if !matches {
                return Err(RouteError::Invalid(
                    "Image contents do not match its PNG, JPEG, WebP or GIF format".to_owned(),
                ));
            }
            input.push(json!({"type":"image","url":url}));
        }
    }
    if input.is_empty() {
        return Err(RouteError::Invalid(
            "Send text or at least one image".to_owned(),
        ));
    }
    Ok(json!(input))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn url(header: &str, bytes: &[u8]) -> String {
        format!("data:{header};base64,{}", STANDARD.encode(bytes))
    }
    #[test]
    fn text_and_image_only_inputs_use_exact_native_arrays() {
        let png = url("image/png", b"\x89PNG\r\n\x1a\n");
        assert_eq!(
            message(&json!({"text":"inspect","images":[png]})).unwrap(),
            json!([{"type":"text","text":"inspect","textElements":[]},{"type":"image","url":png}])
        );
        assert_eq!(
            message(&json!({"images":[png]})).unwrap(),
            json!([{"type":"image","url":png}])
        );
        assert_eq!(
            message(&json!({"text":"legacy"})).unwrap(),
            json!([{"type":"text","text":"legacy","textElements":[]}])
        );
        for (mime, bytes) in [
            ("image/jpeg", &b"\xff\xd8\xff"[..]),
            ("image/gif", &b"GIF89a"[..]),
            ("image/webp", &b"RIFF0000WEBP"[..]),
        ] {
            assert_eq!(
                message(&json!({"images":[url(mime,bytes)]})).unwrap()[0]["type"],
                "image"
            );
        }
    }
    #[test]
    fn empty_malformed_remote_and_wrong_format_inputs_are_refused() {
        for input in [
            json!({}),
            json!({"text":" ","images":[]}),
            json!({"text":null}),
            json!({"text":"bad\0"}),
            json!({"images":null}),
            json!({"images":[{}]}),
            json!({"images":["https://example.test/image.png"]}),
            json!({"images":["/private/file.png"]}),
            json!({"images":["data:image/svg+xml;base64,AAAA"]}),
            json!({"images":["data:image/png;base64,not-base64"]}),
            json!({"images":[url("image/png",b"GIF89a")]}),
            json!({"images":[url("image/png",b"")]}),
        ] {
            assert!(message(&input).is_err(), "invalid input {input}");
        }
    }
    #[test]
    fn invalid_base64_reports_its_distinct_decode_cause() {
        let reason = |encoded: &str| match message(
            &json!({"images":[format!("data:image/png;base64,{encoded}")]}),
        ) {
            Err(RouteError::Invalid(reason)) => reason,
            other => panic!("Expected a base64 input error: {other:?}"),
        };
        let symbol = reason("!!!!");
        let padding = reason("AAAAAAA");
        assert!(symbol.starts_with("Image 1 contains invalid base64:"));
        assert!(padding.starts_with("Image 1 contains invalid base64:"));
        assert_ne!(
            symbol, padding,
            "Distinct native decode failures retain their causes"
        );
    }

    #[test]
    fn image_count_per_file_and_total_byte_bounds_are_exact() {
        let mut bytes = vec![0; IMAGE_BYTES];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        let png = url("image/png", &bytes);
        assert!(message(&json!({"images":[png,png]})).is_ok());
        assert!(
            message(&json!({"images":[png,png,url("image/png",b"\x89PNG\r\n\x1a\n")]})).is_err()
        );
        bytes.push(0);
        assert!(message(&json!({"images":[url("image/png",&bytes)]})).is_err());
        let small = url("image/png", b"\x89PNG\r\n\x1a\n");
        assert!(message(&json!({"images":vec![small.clone();4]})).is_ok());
        assert!(message(&json!({"images":vec![small;5]})).is_err());
    }
}
