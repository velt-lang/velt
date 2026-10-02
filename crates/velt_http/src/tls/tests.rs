//! A TLS round trip against a throwaway CA (the test material of tests/golden/std/_tls.vlt:
//! "Velt Test CA (do not trust)" and a localhost / 127.0.0.1 certificate it signed).

use std::io::{BufReader, Write};
use std::net::TcpListener;

use rustls::pki_types::PrivateKeyDer;
use rustls::{ServerConfig, ServerConnection, StreamOwned};

use super::*;
use crate::client::https;
use crate::message::{read_request, write_response, Response};

const TEST_CA: &str =
  "-----BEGIN CERTIFICATE-----\nMIIBtDCCAVmgAwIBAgIUAdKl7damW41yHgg5amYgxZD1tWcwCgYIKoZIzj0EAwIw\nJjEkMCIGA1UEAwwbVmFpcyBUZXN0IENBIChkbyBub3QgdHJ1c3QpMCAXDTI2MDkz\nMDEzNTcxMloYDzIxMjYwOTA2MTM1NzEyWjAmMSQwIgYDVQQDDBtWYWlzIFRlc3Qg\nQ0EgKGRvIG5vdCB0cnVzdCkwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAASk+j0T\nPdqEZlzKfKYbGiuDdqn5w1BnL2cBv2JUf/NorWzJipuXvAu/LVXoj419DalaZ+4D\nRpiTuT5d5eSRDcndo2MwYTAdBgNVHQ4EFgQU+XUq6wKttKjp0aSG6Tn4kyHKKDow\nHwYDVR0jBBgwFoAU+XUq6wKttKjp0aSG6Tn4kyHKKDowDwYDVR0TAQH/BAUwAwEB\n/zAOBgNVHQ8BAf8EBAMCAQYwCgYIKoZIzj0EAwIDSQAwRgIhALAhEgBiBw5QVGfL\ncr2a+XdMgKEZ6Lgmw/PEFAOL8IZaAiEA7Ba9rrnHO96Vrn8cfkSA6YJbfP4Y8zD5\nRADTuyZ4C9w=\n-----END CERTIFICATE-----\n";
const TEST_CERT: &str =
  "-----BEGIN CERTIFICATE-----\nMIIB4zCCAYmgAwIBAgIUUWjjR76EmcvpdwD5yyBeoGEC+oIwCgYIKoZIzj0EAwIw\nJjEkMCIGA1UEAwwbVmFpcyBUZXN0IENBIChkbyBub3QgdHJ1c3QpMCAXDTI2MDkz\nMDEzNTcxMloYDzIxMjYwOTA2MTM1NzEyWjAUMRIwEAYDVQQDDAlsb2NhbGhvc3Qw\nWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAAS34JkOS/SCH/jEnDTsuUiNpPFqqmgg\nnHmhm9FVPrvW3dzk0y1gwh63NeHjxyHL+KFcX6OAtu2BoAXhCOUH33wxo4GkMIGh\nMCwGA1UdEQQlMCOCCWxvY2FsaG9zdIcEfwAAAYcQAAAAAAAAAAAAAAAAAAAAATAM\nBgNVHRMBAf8EAjAAMA4GA1UdDwEB/wQEAwIHgDATBgNVHSUEDDAKBggrBgEFBQcD\nATAdBgNVHQ4EFgQUBbUl3Qdv7aCuWcSog8ViJH1NEb4wHwYDVR0jBBgwFoAU+XUq\n6wKttKjp0aSG6Tn4kyHKKDowCgYIKoZIzj0EAwIDSAAwRQIhAPnCnjJTXQEYZS2z\nydBhuGIPfgwpZ9Mect34i62AYGYyAiB31JwL5JCmjtIueBktaY+6sJ8hbf1v2xW3\n2KQT7T6l1w==\n-----END CERTIFICATE-----\n";
const TEST_KEY: &str =
  "-----BEGIN PRIVATE KEY-----\nMIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgNMWysN/lz+uQWJ6b\nICqIzZwmPk6pOdOeO30P+ITXNCChRANCAAS34JkOS/SCH/jEnDTsuUiNpPFqqmgg\nnHmhm9FVPrvW3dzk0y1gwh63NeHjxyHL+KFcX6OAtu2BoAXhCOUH33wx\n-----END PRIVATE KEY-----\n";

/// Serve one HTTPS request on a background thread; returns `127.0.0.1:<port>`.
fn serve_once(answer: Response) -> String {
    let certs = CertificateDer::pem_slice_iter(TEST_CERT.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_slice(TEST_KEY.as_bytes()).unwrap();
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let (sock, _) = listener.accept().unwrap();
        let conn = ServerConnection::new(Arc::new(config)).unwrap();
        let mut tls = StreamOwned::new(conn, sock);
        let req = read_request(&mut BufReader::new(&mut tls), 1024);
        let mut answer = answer;
        if let Ok(req) = req {
            answer
                .body
                .extend_from_slice(format!(" {} {}", req.method, req.path).as_bytes());
        }
        let _ = write_response(&mut tls, &answer);
        tls.conn.send_close_notify();
        let _ = tls.flush();
    });
    addr
}

#[test]
fn https_round_trip_with_an_extra_ca() {
    let addr = serve_once(Response::text(201, "made"));
    let config = config_with(TEST_CA.as_bytes()).unwrap();
    let url = format!("{addr}/api/v1/x?q=1");
    let resp = https("PUT", &url, &[("X-A", "b")], b"body", config).unwrap();
    assert_eq!(resp.status, 201);
    assert_eq!(resp.body_text(), "made PUT /api/v1/x");
}

#[test]
fn https_rejects_an_unknown_ca() {
    let addr = serve_once(Response::text(200, "secret"));
    let err = https("GET", &addr, &[], b"", config_with(b"").unwrap()).unwrap_err();
    assert!(err.contains("certificate"), "{err}");
}

#[test]
fn bad_ca_pem_is_reported() {
    let bad = config_with(b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n");
    assert!(bad.is_err());
    assert!(config_with(b"no pem here").is_err());
}
