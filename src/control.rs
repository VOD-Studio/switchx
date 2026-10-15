//! The local control channel through which `switchx mcp --allow-control` asks a
//! running app to preview, apply, or restore Codex configuration. The app
//! listens on loopback and publishes its port with a random token in an
//! owner-only file in the data directory; each connection carries one
//! newline-terminated JSON request and receives one JSON response. The token
//! never appears in responses, logs, or UI.

use std::{
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    path::Path,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use subtle::ConstantTimeEq;

use crate::{app, credentials::Secret};

/// Where a running app publishes its control port and token.
pub const ENDPOINT_FILE: &str = "control-endpoint.json";
/// Requests and responses are small; anything larger is refused.
pub const MESSAGE_LIMIT: usize = 256 * 1024;
/// Previews run the Codex CLI and applies check upstreams, so allow minutes.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(180);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Serialize, Deserialize)]
struct Endpoint {
    port: u16,
    token: String,
}

#[derive(Serialize, Deserialize)]
pub struct Request {
    pub token: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Response {
    Ok { result: Value },
    Error { message: String },
}

impl From<Result<Value, String>> for Response {
    fn from(result: Result<Value, String>) -> Self {
        match result {
            Ok(result) => Self::Ok { result },
            Err(message) => Self::Error { message },
        }
    }
}

/// Publishes a running app's control port with a fresh token, which it returns.
pub fn publish_endpoint(data_dir: &Path, port: u16) -> io::Result<Secret> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| io::Error::other("random source unavailable"))?;
    let token = Secret::new(bytes.iter().map(|byte| format!("{byte:02x}")).collect());
    let mut endpoint = serde_json::to_vec(&Endpoint {
        port,
        token: token.expose().to_owned(),
    })
    .map_err(io::Error::other)?;
    let written = app::replace_private_file(data_dir, ENDPOINT_FILE, &endpoint);
    zeroize::Zeroize::zeroize(&mut endpoint);
    written.map(|()| token)
}

/// Removes the endpoint only while it still names this app, so a newer instance keeps its own.
pub fn remove_endpoint(data_dir: &Path, token: &Secret) {
    if read_endpoint(data_dir).is_ok_and(|endpoint| verify(token.expose(), &endpoint.token)) {
        let _ = fs::remove_file(data_dir.join(ENDPOINT_FILE));
    }
}

fn read_endpoint(data_dir: &Path) -> Result<Endpoint, String> {
    let path = data_dir.join(ENDPOINT_FILE);
    let not_running = || "SwitchX 应用未运行；请先打开 SwitchX，再重试。".to_owned();
    let metadata = fs::symlink_metadata(&path).map_err(|_| not_running())?;
    if !metadata.is_file() || metadata.len() > 4096 {
        return Err(not_running());
    }
    serde_json::from_slice(&fs::read(path).map_err(|_| not_running())?).map_err(|_| not_running())
}

/// Compares tokens in constant time.
pub fn verify(expected: &str, given: &str) -> bool {
    expected.len() == given.len() && bool::from(expected.as_bytes().ct_eq(given.as_bytes()))
}

/// Sends one request to the running app and returns its result.
pub fn call(data_dir: &Path, method: &str, params: Value) -> Result<Value, String> {
    let endpoint = read_endpoint(data_dir)?;
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
    let mut stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
        .map_err(|_| "SwitchX 应用未运行或未响应；请先打开 SwitchX，再重试。".to_owned())?;
    let io_error = |_| "与 SwitchX 应用的连接中断；请确认应用仍在运行后重试。".to_owned();
    stream
        .set_read_timeout(Some(CALL_TIMEOUT))
        .map_err(io_error)?;
    stream
        .set_write_timeout(Some(CONNECT_TIMEOUT))
        .map_err(io_error)?;
    let mut request = serde_json::to_vec(&Request {
        token: endpoint.token,
        method: method.to_owned(),
        params,
    })
    .map_err(|_| "无法编码控制请求".to_owned())?;
    request.push(b'\n');
    let sent = stream.write_all(&request);
    zeroize::Zeroize::zeroize(&mut request);
    sent.map_err(io_error)?;
    let mut line = Vec::new();
    BufReader::new(stream)
        .take(MESSAGE_LIMIT as u64 + 1)
        .read_until(b'\n', &mut line)
        .map_err(|error| {
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) {
                "SwitchX 应用在限定时间内没有完成操作；请在应用中查看结果。".to_owned()
            } else {
                io_error(error)
            }
        })?;
    match serde_json::from_slice::<Response>(&line) {
        Ok(Response::Ok { result }) => Ok(result),
        Ok(Response::Error { message }) => Err(message),
        Err(_) => Err("SwitchX 应用返回了无法识别的响应；请确认应用与命令为同一版本。".into()),
    }
}

#[cfg(test)]
mod tests {
    use std::{env, net::TcpListener, thread};

    use serde_json::json;

    use super::*;

    struct Data(std::path::PathBuf);

    impl Data {
        fn new() -> Self {
            let mut nonce = [0_u8; 8];
            getrandom::fill(&mut nonce).unwrap();
            let suffix: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
            let path = env::temp_dir().join(format!("switchx-control-{suffix}"));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Data {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Answers one connection the way the app does, checking the token.
    fn serve_once(listener: TcpListener, token: String, reply: Response) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Request = serde_json::from_str(&line).unwrap();
            let response = if verify(&token, &request.token) {
                assert_eq!(request.method, "status");
                assert_eq!(request.params, json!({ "home": "/tmp/codex" }));
                reply
            } else {
                Response::Error {
                    message: "unauthorized".into(),
                }
            };
            let mut stream = stream;
            serde_json::to_writer(&mut stream, &response).unwrap();
            stream.write_all(b"\n").unwrap();
        })
    }

    #[test]
    fn calls_reach_the_published_endpoint_with_its_token() {
        let data = Data::new();
        assert!(
            call(&data.0, "status", json!({}))
                .unwrap_err()
                .contains("未运行")
        );

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let token = publish_endpoint(&data.0, port).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(data.0.join(ENDPOINT_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let server = serve_once(
            listener,
            token.expose().to_owned(),
            Response::Ok {
                result: json!({ "running": true }),
            },
        );
        let result = call(&data.0, "status", json!({ "home": "/tmp/codex" })).unwrap();
        server.join().unwrap();
        assert_eq!(result, json!({ "running": true }));

        // A stale token is refused, and the error never echoes it.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
        let server = serve_once(listener, "0".repeat(64), Response::Ok { result: json!({}) });
        let error = call(&data.0, "status", json!({ "home": "/tmp/codex" })).unwrap_err();
        server.join().unwrap();
        assert_eq!(error, "unauthorized");
        assert!(!error.contains(token.expose()));
    }

    #[test]
    fn endpoint_removal_keeps_a_newer_instance() {
        let data = Data::new();
        let old = publish_endpoint(&data.0, 1).unwrap();
        let new = publish_endpoint(&data.0, 2).unwrap();
        remove_endpoint(&data.0, &old);
        assert!(data.0.join(ENDPOINT_FILE).exists());
        remove_endpoint(&data.0, &new);
        assert!(!data.0.join(ENDPOINT_FILE).exists());
        assert!(verify("abc", "abc"));
        assert!(!verify("abc", "abd"));
        assert!(!verify("abc", "ab"));
    }
}
