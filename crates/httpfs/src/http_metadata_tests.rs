#![allow(clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

use roastedduck_common::{Error, ErrorKind};

fn agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .proxy(None)
            .timeout_connect(Some(Duration::from_secs(3)))
            .timeout_per_read(Some(Duration::from_secs(3)))
            .build(),
    )
}

fn read_request(stream: &std::net::TcpStream) {
    let mut input = BufReader::new(stream);
    loop {
        let mut line = String::new();
        input.read_line(&mut line).unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
    }
}

#[test]
fn response_metadata_survives_normalization_and_response_drop() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&stream);
        stream
            .write_all(
                b"HTTP/1.1 429 Slow Down\r\nx-MiXeD: first\r\nX-MIXED: second\r\nContent-Length: 4\r\n\r\nfail",
            )
            .unwrap();
    });
    let response = agent().get(format!("http://{address}/missing")).call().unwrap();
    let wire = response
        .extensions()
        .get::<ureq_proto::OriginalResponseMetadata>()
        .unwrap();
    assert_eq!(wire.reason, "Slow Down");
    let error = super::response_error(Error::new(ErrorKind::Http, "rate limited"), &response, b"", false);
    drop(response);
    server.join().unwrap();
    let info = error.extra_info();
    assert!(info.contains(&("status_code", b"429".as_slice())));
    assert!(info.contains(&("reason", b"Too Many Requests".as_slice())));
    assert!(info.contains(&("response_body", b"".as_slice())));
    assert!(info.contains(&("header_x-MiXeD", b"first".as_slice())));
    assert!(!info.iter().any(|(name, _)| *name == "header_X-MIXED"));
}

#[test]
fn capturing_headers_does_not_wait_for_or_consume_the_success_body() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let (sent, received) = mpsc::channel();
    let payload = vec![b'x'; 4096];
    let expected = payload.clone();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&stream);
        stream
            .write_all(b"HTTP/1.1 200 Fine\r\nContent-Length: 4096\r\nx-MiXeD: value\r\n\r\n")
            .unwrap();
        stream.flush().unwrap();
        received.recv_timeout(Duration::from_secs(3)).unwrap();
        stream.write_all(&payload).unwrap();
    });
    let mut response = agent().get(format!("http://{address}/data")).call().unwrap();
    let metadata = super::take_response_metadata(&mut response, true);
    assert_eq!(metadata.reason, "Fine");
    assert!(
        metadata
            .headers
            .contains(&("x-MiXeD".to_owned(), b"value".to_vec()))
    );
    sent.send(()).unwrap();
    let mut actual = Vec::new();
    response.body_mut().as_reader().read_to_end(&mut actual).unwrap();
    server.join().unwrap();
    assert_eq!(actual, expected);
}

// Golden JSON payloads are captured from authenticated C++934/API in evidence1603.
#[cfg(target_vendor = "apple")]
fn http_json_profile(httplib: bool, positioned: bool, expected: &[u8]) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&stream);
        stream.write_all(b"HTTP/1.1 404 Not Found\r\nServer: BaseHTTP/0.6 Python/3.10.16\r\nDate: Wed, 01 Jan 2020 00:00:00 GMT\r\nContent-Length: 33\r\nETag: \"duckdb-python-extension-fixture-v1\"\r\nX-Parity-Fixture: 1522\r\n\r\nThe extension fixture is missing\n").unwrap();
    });
    let response = agent().get(format!("http://{address}/missing")).call().unwrap();
    let body = if positioned {
        b"".as_slice()
    } else {
        b"The extension fixture is missing\n".as_slice()
    };
    let mut error = super::response_error(Error::new(ErrorKind::Http, "m"), &response, body, httplib);
    // The file bind catches before AddQueryLocation; PhysicalLoad instead
    // reaches the executor-task catch. Constructing HTTPException does neither.
    error = error.caught();
    if positioned {
        error = error.with_position(Some(14));
    }
    drop(response);
    server.join().unwrap();
    assert_eq!(error.into_json().to_bytes(), expected);
}

#[test]
#[cfg(target_vendor = "apple")]
fn file_response_json_matches_native_cpp_metadata_order() {
    http_json_profile(false, true, b"{\"exception_type\":\"HTTP\",\"exception_message\":\"m\",\"response_body\":\"\",\"header_Server\":\"BaseHTTP/0.6 Python/3.10.16\",\"header_Date\":\"Wed, 01 Jan 2020 00:00:00 GMT\",\"header_Content-Length\":\"33\",\"header_X-Parity-Fixture\":\"1522\",\"status_code\":\"404\",\"reason\":\"Not Found\",\"header_ETag\":\"\\\"duckdb-python-extension-fixture-v1\\\"\",\"position\":\"14\"}");
}

#[test]
#[cfg(target_vendor = "apple")]
fn installer_response_json_matches_native_cpp_metadata_order() {
    http_json_profile(true, false, b"{\"exception_type\":\"HTTP\",\"exception_message\":\"m\",\"response_body\":\"The extension fixture is missing\\n\",\"header_Server\":\"BaseHTTP/0.6 Python/3.10.16\",\"header_Date\":\"Wed, 01 Jan 2020 00:00:00 GMT\",\"header_X-Parity-Fixture\":\"1522\",\"status_code\":\"404\",\"header_Content-Length\":\"33\",\"header_ETag\":\"\\\"duckdb-python-extension-fixture-v1\\\"\",\"reason\":\"Not Found\"}");
}
