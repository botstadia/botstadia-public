//! Attestation tokens. In an image they come from Confidential Space's
//! launcher, over the socket only the workload's container reaches; locally
//! from the mock CA, whose root only the local deploy approves.

/// Takes a fresh token whose nonces bind the given values.
pub trait Attester {
    fn token(&self, nonces: &[String], now_s: u64) -> anyhow::Result<String>;
}

/// The local target's attester (feature `mock`).
#[cfg(feature = "mock")]
pub struct MockAttester;

#[cfg(feature = "mock")]
impl Attester for MockAttester {
    fn token(&self, nonces: &[String], now_s: u64) -> anyhow::Result<String> {
        Ok(botstadia_attestation_core::mock::issue(nonces, now_s))
    }
}

/// Where Confidential Space's launcher answers the workload's token
/// requests.
pub const LAUNCHER_SOCKET: &str = "/run/container_launcher/teeserver.sock";

/// The kind of token asked for: a PKI token, whose header carries its
/// signing key's certificate chain up to Google's Confidential Space root,
/// which the guest checks against an approved GoogleRootCert
/// (TRUST.md#the-tee-and-the-zk-proof).
pub const TOKEN_TYPE: &str = "PKI";

/// The launcher's answer is a JWT of a few KiB; anything far longer is
/// refused rather than read.
const ANSWER_LIMIT: usize = 64 * 1024;

/// Confidential Space's launcher: a fresh PKI token of our audience whose
/// `eat_nonce` carries the given nonces (at most six, each 10 to 74 bytes,
/// the launcher's own limits).
pub struct Launcher {
    pub socket: std::path::PathBuf,
}

impl Default for Launcher {
    fn default() -> Self {
        Self {
            socket: LAUNCHER_SOCKET.into(),
        }
    }
}

impl Attester for Launcher {
    fn token(&self, nonces: &[String], _now_s: u64) -> anyhow::Result<String> {
        anyhow::ensure!(
            (1..=6).contains(&nonces.len()),
            "the launcher takes one to six nonces, not {}",
            nonces.len()
        );
        for n in nonces {
            anyhow::ensure!(
                (10..=74).contains(&n.len()),
                "a launcher nonce is 10 to 74 bytes, not {}",
                n.len()
            );
        }
        let body = serde_json::json!({
            "audience": botstadia_attestation_core::AUDIENCE,
            "token_type": TOKEN_TYPE,
            "nonces": nonces,
        })
        .to_string();
        let answer = unix_post(&self.socket, "/v1/token", &body)?;
        let token = answer.trim();
        anyhow::ensure!(
            token.split('.').count() == 3,
            "the launcher's answer isn't a JWT"
        );
        Ok(token.to_string())
    }
}

/// One HTTP/1.1 POST over a Unix socket, read to the end (the launcher is
/// Go's `net/http`, so the body comes with a length or chunked).
#[cfg(unix)]
fn unix_post(socket: &std::path::Path, path: &str, body: &str) -> anyhow::Result<String> {
    use std::io::{Read, Write};
    use std::time::Duration;

    let mut stream = std::os::unix::net::UnixStream::connect(socket)
        .map_err(|e| anyhow::anyhow!("connecting to the launcher at {}: {e}", socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    let mut raw = Vec::new();
    stream
        .take(ANSWER_LIMIT as u64 + 4096)
        .read_to_end(&mut raw)?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("the launcher's answer has no headers"))?;
    let head = std::str::from_utf8(&raw[..split])?;
    let mut rest = raw[split + 4..].to_vec();
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| anyhow::anyhow!("the launcher's answer has no status"))?;
    let chunked = head.lines().any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    if chunked {
        rest = dechunk(&rest)?;
    }
    anyhow::ensure!(
        rest.len() <= ANSWER_LIMIT,
        "the launcher's answer is too long"
    );
    let text =
        String::from_utf8(rest).map_err(|_| anyhow::anyhow!("the launcher's answer isn't text"))?;
    anyhow::ensure!(
        (200..300).contains(&status),
        "the launcher refused the token request ({status}): {}",
        text.chars().take(200).collect::<String>()
    );
    Ok(text)
}

#[cfg(not(unix))]
fn unix_post(_: &std::path::Path, _: &str, _: &str) -> anyhow::Result<String> {
    anyhow::bail!("the launcher's socket needs Unix")
}

/// A chunked body, joined.
fn dechunk(mut raw: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line = raw
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| anyhow::anyhow!("a broken chunk"))?;
        let size = std::str::from_utf8(&raw[..line])?;
        let size = usize::from_str_radix(size.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| anyhow::anyhow!("a broken chunk size"))?;
        raw = &raw[line + 2..];
        if size == 0 {
            return Ok(out);
        }
        anyhow::ensure!(raw.len() >= size + 2, "a short chunk");
        out.extend_from_slice(&raw[..size]);
        anyhow::ensure!(
            out.len() <= ANSWER_LIMIT,
            "the launcher's answer is too long"
        );
        raw = &raw[size + 2..];
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// A launcher that answers one request with `answer`, handing back what
    /// it was asked.
    fn launcher(answer: &'static str) -> (Launcher, std::thread::JoinHandle<String>) {
        let dir = std::env::temp_dir().join(format!(
            "botstadia-launcher-{}-{}",
            std::process::id(),
            answer.len()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("teeserver.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = vec![0u8; 8192];
            let mut got = Vec::new();
            loop {
                let n = s.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got);
                if let Some(i) = text.find("\r\n\r\n") {
                    let len: usize = text
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if got.len() >= i + 4 + len {
                        break;
                    }
                }
            }
            s.write_all(answer.as_bytes()).unwrap();
            String::from_utf8(got).unwrap()
        });
        (Launcher { socket }, handle)
    }

    fn nonce() -> String {
        "ab".repeat(32)
    }

    #[test]
    fn asks_for_a_pki_token_of_our_audience_with_the_nonces() {
        let (l, asked) = launcher("HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\na.b.c\n");
        assert_eq!(l.token(&[nonce()], 0).unwrap(), "a.b.c");
        let asked = asked.join().unwrap();
        assert!(asked.starts_with("POST /v1/token HTTP/1.1\r\n"));
        let body: serde_json::Value =
            serde_json::from_str(&asked[asked.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(body["audience"], botstadia_attestation_core::AUDIENCE);
        assert_eq!(body["token_type"], "PKI");
        assert_eq!(body["nonces"], serde_json::json!([nonce()]));
    }

    #[test]
    fn reads_a_chunked_answer() {
        let (l, _) = launcher(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\na.b\r\n2\r\n.c\r\n0\r\n\r\n",
        );
        assert_eq!(l.token(&[nonce()], 0).unwrap(), "a.b.c");
    }

    #[test]
    fn a_refusal_or_a_non_token_is_an_error() {
        let (l, _) = launcher("HTTP/1.1 400 Bad Request\r\nContent-Length: 3\r\n\r\nno\n");
        assert!(
            l.token(&[nonce()], 0)
                .unwrap_err()
                .to_string()
                .contains("400")
        );
        let (l, _) = launcher("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi");
        assert!(l.token(&[nonce()], 0).is_err());
    }

    #[test]
    fn nonces_outside_the_launcher_s_limits_are_refused_before_asking() {
        let l = Launcher {
            socket: "/nonexistent".into(),
        };
        assert!(
            l.token(&[], 0)
                .unwrap_err()
                .to_string()
                .contains("one to six")
        );
        assert!(
            l.token(&["short".into()], 0)
                .unwrap_err()
                .to_string()
                .contains("10 to 74")
        );
        assert!(l.token(&vec![nonce(); 7], 0).is_err());
    }
}
