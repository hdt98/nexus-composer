//! HTTP content-encoding utilities.
//!
//! Reqwest automatic decompression is disabled to preserve `accept-encoding`, so bodies are
//! decompressed explicitly. Requests such as authenticated Codex Desktop bodies and compressed
//! upstream responses share this logic.

use axum::http::header::HeaderMap;
use std::io::Read;

/// Splits content-encoding into ordered codings, excluding `identity` and empty values.
///
/// HTTP permits comma-separated stacked codings such as `gzip, zstd` and repeated
/// content-encoding headers with equivalent semantics. See [`get_content_encoding`].
fn split_codings(content_encoding: &str) -> Vec<&str> {
    content_encoding
        .split(',')
        .map(str::trim)
        .filter(|c| !c.is_empty() && *c != "identity")
        .collect()
}

/// Returns whether one coding can be decompressed.
fn is_single_supported(coding: &str) -> bool {
    matches!(
        coding,
        "gzip" | "x-gzip" | "deflate" | "br" | "zstd" | "zst"
    )
}

/// Decompresses one content coding. Unknown codings return `Ok(None)`.
fn decompress_single(coding: &str, body: &[u8]) -> Result<Option<Vec<u8>>, std::io::Error> {
    match coding {
        "gzip" | "x-gzip" => {
            let mut decoder = flate2::read::GzDecoder::new(body);
            let mut decompressed = Vec::new();
            decoder.read_to_end(&mut decompressed)?;
            Ok(Some(decompressed))
        }
        "deflate" => {
            // RFC 9110 defines deflate as zlib-wrapped, but some upstreams and clients send raw
            // deflate streams. Try zlib first, then raw; otherwise compliant sources would fail
            // and compressed bytes could fail open into JSON parsing (#2234 case C).
            let mut decompressed = Vec::new();
            let mut zlib = flate2::read::ZlibDecoder::new(body);
            match zlib.read_to_end(&mut decompressed) {
                Ok(_) => Ok(Some(decompressed)),
                Err(zlib_err) => {
                    log::debug!(
                        "Deflate zlib decompression failed ({zlib_err}); falling back to raw deflate"
                    );
                    let mut decompressed = Vec::new();
                    let mut raw = flate2::read::DeflateDecoder::new(body);
                    raw.read_to_end(&mut decompressed)?;
                    Ok(Some(decompressed))
                }
            }
        }
        "br" => {
            let mut decompressed = Vec::new();
            brotli::BrotliDecompress(&mut std::io::Cursor::new(body), &mut decompressed)?;
            Ok(Some(decompressed))
        }
        "zstd" | "zst" => {
            // Authenticated Codex requests use zstd (`Compression::Zstd`), and upstreams may
            // also return zstd-compressed responses.
            let decompressed = zstd::stream::decode_all(std::io::Cursor::new(body))?;
            Ok(Some(decompressed))
        }
        _ => Ok(None),
    }
}

/// Decompresses body bytes according to content-encoding, including stacked codings.
///
/// RFC 9110 §8.4 lists codings in application order, so decoding occurs in reverse.
/// `Ok(None)` means an unsupported coding was present and bytes remain unchanged. The caller
/// must then preserve the content-encoding header so downstream consumers do not interpret
/// compressed bytes as plaintext.
pub(crate) fn decompress_body(
    content_encoding: &str,
    body: &[u8],
) -> Result<Option<Vec<u8>>, std::io::Error> {
    let codings = split_codings(content_encoding);
    if codings.is_empty() {
        return Ok(None);
    }
    // If any coding is unsupported, preserve the encoded body and header to avoid partial data.
    if !codings.iter().all(|c| is_single_supported(c)) {
        log::warn!("Unsupported content-encoding: {content_encoding}; skipping decompression");
        return Ok(None);
    }

    // Decode in reverse because the last coding listed was applied last.
    let mut data: Option<Vec<u8>> = None;
    for coding in codings.iter().rev() {
        let input = data.as_deref().unwrap_or(body);
        match decompress_single(coding, input)? {
            Some(decompressed) => data = Some(decompressed),
            // Defensive fallback after validation by `is_single_supported`.
            None => return Ok(None),
        }
    }
    Ok(data)
}

/// Returns whether all codings in content-encoding can be decompressed.
///
/// Request handling uses this as a gate: reject compressed bodies that cannot be decoded
/// before JSON parsing.
pub(crate) fn is_supported_content_encoding(content_encoding: &str) -> bool {
    let codings = split_codings(content_encoding);
    !codings.is_empty() && codings.iter().all(|c| is_single_supported(c))
}

/// Extracts content-encoding from headers, merging duplicates and ignoring empty or identity.
///
/// HTTP permits repeated content-encoding headers with comma-joined semantics, so `get_all`
/// combines them. [`decompress_body`] decodes the resulting list in reverse.
pub(crate) fn get_content_encoding(headers: &HeaderMap) -> Option<String> {
    let combined = headers
        .get_all("content-encoding")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
        .to_lowercase();
    if split_codings(&combined).is_empty() {
        return None;
    }
    Some(combined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn decompress_body_deflate_handles_zlib_wrapped_per_rfc9110() {
        // RFC 9110-compliant deflate uses zlib wrapping.
        let payload = br#"{"ok":true}"#;
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, payload).unwrap();
        let compressed = encoder.finish().unwrap();

        let decompressed = decompress_body("deflate", &compressed).unwrap().unwrap();
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn decompress_body_deflate_falls_back_to_raw_stream() {
        // Retain compatibility with sources that send raw deflate streams.
        let payload = br#"{"ok":true}"#;
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, payload).unwrap();
        let compressed = encoder.finish().unwrap();

        let decompressed = decompress_body("deflate", &compressed).unwrap().unwrap();
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn decompress_body_zstd_roundtrip() {
        // Authenticated Codex sends zstd-compressed request bodies.
        let payload = br#"{"hello":"world","n":42}"#;
        let compressed = zstd::stream::encode_all(std::io::Cursor::new(&payload[..]), 0).unwrap();
        let decompressed = decompress_body("zstd", &compressed).unwrap().unwrap();
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn decompress_body_stacked_gzip_then_zstd_decodes_in_reverse() {
        // `Content-Encoding: gzip, zstd` means gzip then zstd, so decode zstd then gzip.
        let payload = br#"{"stacked":true}"#;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, payload).unwrap();
        let gzipped = gz.finish().unwrap();
        let stacked = zstd::stream::encode_all(std::io::Cursor::new(&gzipped[..]), 0).unwrap();

        let decompressed = decompress_body("gzip, zstd", &stacked).unwrap().unwrap();
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn decompress_body_stacked_with_unsupported_returns_none() {
        // Preserve the complete encoded body and header if any stacked coding is unsupported.
        let result = decompress_body("snappy, zstd", b"\x00\x01\x02\x03").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn decompress_body_unknown_encoding_returns_none_to_keep_headers() {
        // Unknown codings must return None rather than appearing decoded; otherwise the header
        // would be removed and downstream diagnostics would treat compressed bytes as plaintext.
        let result = decompress_body("snappy", b"\x00\x01\x02\x03").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn is_supported_content_encoding_matches_decompressable() {
        for enc in [
            "gzip",
            "x-gzip",
            "deflate",
            "br",
            "zstd",
            "zst",
            "gzip, zstd",
        ] {
            assert!(
                is_supported_content_encoding(enc),
                "{enc} should be supported"
            );
        }
        for enc in ["identity", "snappy", "compress", "", "gzip, snappy"] {
            assert!(
                !is_supported_content_encoding(enc),
                "{enc} should not be supported"
            );
        }
    }

    #[test]
    fn get_content_encoding_combines_repeated_headers() {
        // Repeated content-encoding headers are comma-joined and must be merged with `get_all`.
        let mut headers = HeaderMap::new();
        headers.append("content-encoding", HeaderValue::from_static("gzip"));
        headers.append("content-encoding", HeaderValue::from_static("zstd"));
        assert_eq!(
            get_content_encoding(&headers).as_deref(),
            Some("gzip, zstd")
        );
    }

    #[test]
    fn get_content_encoding_ignores_identity_only() {
        let mut headers = HeaderMap::new();
        headers.append("content-encoding", HeaderValue::from_static("identity"));
        assert_eq!(get_content_encoding(&headers), None);
    }
}
