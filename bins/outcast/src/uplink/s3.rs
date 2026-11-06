//! Signature Version 4 for an S3-compatible origin (AWS S3, Cloudflare R2,
//! Backblaze B2, MinIO, Wasabi): the request's canonical form hashed and
//! signed with the key derived from the secret, the date, the region and
//! the service, as AWS specifies; ring's SHA-256 and HMAC do the arithmetic.

use std::time::{SystemTime, UNIX_EPOCH};

use ring::{digest, hmac};

/// The credentials and the scope a request is signed for.
#[derive(Clone)]
pub struct Signer {
    pub access_key: String,
    pub secret_key: String,
    pub region: String,
}

/// Bytes as lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(digest::digest(&digest::SHA256, bytes).as_ref())
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data)
        .as_ref()
        .to_vec()
}

/// A moment as `YYYYMMDDTHHMMSSZ`, the form `x-amz-date` takes.
pub fn amz_date(seconds: u64) -> String {
    let (y, m, d, hh, mm, ss) = civil(seconds);
    format!("{y:04}{m:02}{d:02}T{hh:02}{mm:02}{ss:02}Z")
}

pub fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Civil time from seconds since the epoch (Howard Hinnant's algorithm).
pub fn civil(seconds: u64) -> (i64, i64, i64, i64, i64, i64) {
    let whole = seconds as i64;
    let days = whole.div_euclid(86_400);
    let secs = whole.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, secs / 3600, (secs % 3600) / 60, secs % 60)
}

/// The characters S3 leaves bare in a URI; everything else is percent-encoded.
fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let bare = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'~')
            || (keep_slash && b == b'/');
        if bare {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl Signer {
    /// The `Authorization` value for a request: `method` on `path` (the URI path,
    /// bare), with `headers` (name and value, every one of them signed, the host
    /// among them) and the payload's SHA-256 in hex, at `amz_date`.
    pub fn authorization(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        payload_sha256: &str,
        amz_date: &str,
    ) -> String {
        let mut sorted: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        sorted.sort();
        let canonical_headers: String = sorted.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
        let signed_headers: String = sorted
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>()
            .join(";");
        let canonical_request = format!(
            "{method}\n{}\n\n{canonical_headers}\n{signed_headers}\n{payload_sha256}",
            uri_encode(path, true)
        );
        let date = &amz_date[..8];
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            sha256_hex(canonical_request.as_bytes())
        );
        let k_date = hmac_sha256(
            format!("AWS4{}", self.secret_key).as_bytes(),
            date.as_bytes(),
        );
        let k_region = hmac_sha256(&k_date, self.region.as_bytes());
        let k_service = hmac_sha256(&k_region, b"s3");
        let k_signing = hmac_sha256(&k_service, b"aws4_request");
        let signature = hex(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            self.access_key
        )
    }

    /// The headers a request carries, signed: the host, the payload's hash, the
    /// date, and `Authorization` last.
    pub fn headers(
        &self,
        method: &str,
        path: &str,
        host: &str,
        body: &[u8],
        now: u64,
    ) -> Vec<(String, String)> {
        let payload = sha256_hex(body);
        let date = amz_date(now);
        let signed = [
            ("host", host),
            ("x-amz-content-sha256", payload.as_str()),
            ("x-amz-date", date.as_str()),
        ];
        let auth = self.authorization(method, path, &signed, &payload, &date);
        vec![
            ("x-amz-content-sha256".to_string(), payload),
            ("x-amz-date".to_string(), date),
            ("Authorization".to_string(), auth),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The worked example of AWS's own documentation ("Signature Calculations for
    // the Authorization Header: Transferring Payload in a Single Chunk", the GET
    // object example with its test credentials).
    fn example() -> Signer {
        Signer {
            access_key: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            region: "us-east-1".into(),
        }
    }

    #[test]
    fn the_documented_get_object_signature_comes_out() {
        let empty = sha256_hex(b"");
        assert_eq!(
            empty,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let auth = example().authorization(
            "GET",
            "/test.txt",
            &[
                ("Host", "examplebucket.s3.amazonaws.com"),
                ("Range", "bytes=0-9"),
                ("x-amz-content-sha256", &empty),
                ("x-amz-date", "20130524T000000Z"),
            ],
            &empty,
            "20130524T000000Z",
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn the_documented_put_object_signature_comes_out() {
        let body = b"Welcome to Amazon S3.";
        let payload = sha256_hex(body);
        assert_eq!(
            payload,
            "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072"
        );
        let auth = example().authorization(
            "PUT",
            "/test$file.text",
            &[
                ("Date", "Fri, 24 May 2013 00:00:00 GMT"),
                ("Host", "examplebucket.s3.amazonaws.com"),
                ("x-amz-content-sha256", &payload),
                ("x-amz-date", "20130524T000000Z"),
                ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
            ],
            &payload,
            "20130524T000000Z",
        );
        assert!(
            auth.ends_with(
                "Signature=98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
            ),
            "{auth}"
        );
    }

    #[test]
    fn dates_and_paths_take_their_forms() {
        assert_eq!(amz_date(1_369_353_600), "20130524T000000Z");
        assert_eq!(uri_encode("/radio/live-7.aac", true), "/radio/live-7.aac");
        assert_eq!(
            uri_encode("/my station/a b.m3u8", true),
            "/my%20station/a%20b.m3u8"
        );
        let h = example().headers(
            "PUT",
            "/radio/x.aac",
            "s3.example.com",
            b"abc",
            1_369_353_600,
        );
        assert_eq!(h[0].0, "x-amz-content-sha256");
        assert_eq!(
            h[1],
            ("x-amz-date".to_string(), "20130524T000000Z".to_string())
        );
        assert!(h[2].1.starts_with("AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature="));
    }
}
