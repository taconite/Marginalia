use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PiSession {
    socket_path: String,
    cwd: String,
    pid: u32,
    created_at: u64,
}

pub fn list_pi_sessions() -> Result<Vec<PiSession>, String> {
    platform::list_pi_sessions()
}

pub fn send_pi_prompt(socket_path: &str, message: &str) -> Result<(), String> {
    platform::send_pi_prompt(socket_path, message)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod platform {
    use super::PiSession;
    use serde::Deserialize;
    use serde_json::{json, Value};
    use std::ffi::OsString;
    use std::fs::{self, Metadata};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    const MAX_INFO_BYTES: u64 = 64 * 1024;
    const SOCKET_TIMEOUT: Duration = Duration::from_secs(2);

    #[derive(Deserialize)]
    struct SessionInfo {
        socket_path: String,
        cwd: String,
        pid: u32,
        created_at: u64,
    }

    #[derive(Deserialize)]
    struct SocketResponse {
        ok: bool,
        error: Option<String>,
    }

    pub fn list_pi_sessions() -> Result<Vec<PiSession>, String> {
        list_pi_sessions_at(&runtime_dir())
    }

    pub fn send_pi_prompt(socket_path: &str, message: &str) -> Result<(), String> {
        if message.trim().is_empty() {
            return Err("Prompt cannot be empty".to_string());
        }

        let runtime_dir = runtime_dir();
        validate_runtime_dir(&runtime_dir)?;
        let info_dir = runtime_dir.join("sockets");
        validate_info_dir(&info_dir)?;

        let socket_path = PathBuf::from(socket_path);
        validate_session_socket(&info_dir, &socket_path)?;
        send_request(
            &socket_path,
            &json!({ "type": "prompt", "message": message }),
        )
    }

    fn runtime_dir() -> PathBuf {
        match std::env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty()) {
            Some(path) => PathBuf::from(path).join("pi-nvim"),
            None => std::env::temp_dir().join(format!("pi-nvim-{}", current_uid())),
        }
    }

    fn current_uid() -> u32 {
        // SAFETY: geteuid has no preconditions and does not dereference pointers.
        unsafe { libc::geteuid() }
    }

    fn trusted_owner(metadata: &Metadata) -> bool {
        metadata.uid() == current_uid()
    }

    fn validate_runtime_dir(path: &Path) -> Result<(), String> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("Pi runtime directory is unavailable: {error}"))?;
        if !metadata.is_dir() || !trusted_owner(&metadata) {
            return Err("Pi runtime directory is not a trusted directory".to_string());
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("Pi runtime directory is not private".to_string());
        }
        Ok(())
    }

    fn validate_info_dir(path: &Path) -> Result<(), String> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("Pi session directory is unavailable: {error}"))?;
        if !metadata.is_dir() || !trusted_owner(&metadata) {
            return Err("Pi session directory is not trusted".to_string());
        }
        Ok(())
    }

    fn info_path_for_socket(socket_path: &Path) -> PathBuf {
        let mut value: OsString = socket_path.as_os_str().to_owned();
        value.push(".info");
        PathBuf::from(value)
    }

    fn read_session_info(info_dir: &Path, info_path: &Path) -> Result<SessionInfo, String> {
        let metadata = fs::symlink_metadata(info_path)
            .map_err(|error| format!("Cannot inspect session metadata: {error}"))?;
        if !metadata.is_file() || !trusted_owner(&metadata) || metadata.len() > MAX_INFO_BYTES {
            return Err("Session metadata is not trusted".to_string());
        }

        let contents = fs::read_to_string(info_path)
            .map_err(|error| format!("Cannot read session metadata: {error}"))?;
        let info: SessionInfo = serde_json::from_str(&contents)
            .map_err(|error| format!("Invalid session metadata: {error}"))?;
        let socket_path = Path::new(&info.socket_path);

        if socket_path.parent() != Some(info_dir) || info_path_for_socket(socket_path) != info_path
        {
            return Err("Session metadata points outside the Pi session directory".to_string());
        }

        Ok(info)
    }

    fn validate_socket(path: &Path) -> Result<(), String> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("Pi socket is unavailable: {error}"))?;
        if !metadata.file_type().is_socket() || !trusted_owner(&metadata) {
            return Err("Pi socket is not trusted".to_string());
        }
        Ok(())
    }

    fn validate_session_socket(info_dir: &Path, socket_path: &Path) -> Result<(), String> {
        if socket_path.parent() != Some(info_dir) {
            return Err("Pi socket is outside the session directory".to_string());
        }
        let info_path = info_path_for_socket(socket_path);
        let info = read_session_info(info_dir, &info_path)?;
        if Path::new(&info.socket_path) != socket_path {
            return Err("Pi session metadata does not match the selected socket".to_string());
        }
        validate_socket(socket_path)
    }

    fn send_request(socket_path: &Path, request: &Value) -> Result<(), String> {
        let mut stream = UnixStream::connect(socket_path)
            .map_err(|error| format!("Cannot connect to Pi: {error}"))?;
        stream
            .set_read_timeout(Some(SOCKET_TIMEOUT))
            .map_err(|error| format!("Cannot configure Pi socket: {error}"))?;
        stream
            .set_write_timeout(Some(SOCKET_TIMEOUT))
            .map_err(|error| format!("Cannot configure Pi socket: {error}"))?;

        serde_json::to_writer(&mut stream, request)
            .map_err(|error| format!("Cannot encode Pi request: {error}"))?;
        stream
            .write_all(b"\n")
            .map_err(|error| format!("Cannot send request to Pi: {error}"))?;
        stream
            .flush()
            .map_err(|error| format!("Cannot flush request to Pi: {error}"))?;

        let mut response_line = String::new();
        BufReader::new(stream)
            .read_line(&mut response_line)
            .map_err(|error| format!("Cannot read Pi response: {error}"))?;
        if response_line.is_empty() {
            return Err("Pi closed the socket without responding".to_string());
        }

        let response: SocketResponse = serde_json::from_str(&response_line)
            .map_err(|error| format!("Pi returned invalid JSON: {error}"))?;
        if !response.ok {
            return Err(response
                .error
                .unwrap_or_else(|| "Pi rejected the request".to_string()));
        }
        Ok(())
    }

    fn list_pi_sessions_at(runtime_dir: &Path) -> Result<Vec<PiSession>, String> {
        if !runtime_dir.exists() {
            return Ok(Vec::new());
        }
        validate_runtime_dir(runtime_dir)?;

        let info_dir = runtime_dir.join("sockets");
        if !info_dir.exists() {
            return Ok(Vec::new());
        }
        validate_info_dir(&info_dir)?;

        let entries =
            fs::read_dir(&info_dir).map_err(|error| format!("Cannot list Pi sessions: {error}"))?;
        let mut sessions = Vec::new();

        for entry in entries.flatten() {
            let info_path = entry.path();
            if info_path.extension().and_then(|value| value.to_str()) != Some("info") {
                continue;
            }
            let Ok(info) = read_session_info(&info_dir, &info_path) else {
                continue;
            };
            let socket_path = PathBuf::from(&info.socket_path);
            if validate_socket(&socket_path).is_err()
                || send_request(&socket_path, &json!({ "type": "ping" })).is_err()
            {
                continue;
            }

            sessions.push(PiSession {
                socket_path: info.socket_path,
                cwd: info.cwd,
                pid: info.pid,
                created_at: info.created_at,
            });
        }

        sessions.sort_by_key(|session| std::cmp::Reverse(session.created_at));
        Ok(sessions)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::unix::net::UnixListener;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::thread;
        use std::time::{SystemTime, UNIX_EPOCH};

        static NEXT_ID: AtomicU64 = AtomicU64::new(0);

        fn test_runtime_dir() -> PathBuf {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock before Unix epoch")
                .as_nanos();
            std::env::temp_dir().join(format!(
                "marginalia-pi-bridge-{}-{timestamp}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ))
        }

        fn create_runtime() -> (PathBuf, PathBuf) {
            let runtime_dir = test_runtime_dir();
            let info_dir = runtime_dir.join("sockets");
            fs::create_dir_all(&info_dir).expect("create test runtime");
            fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o700))
                .expect("secure test runtime");
            fs::set_permissions(&info_dir, fs::Permissions::from_mode(0o700))
                .expect("secure test session directory");
            (runtime_dir, info_dir)
        }

        fn write_info(info_dir: &Path, socket_path: &Path, created_at: u64) {
            let info = json!({
                "socket_path": socket_path,
                "cwd": "/work/paper",
                "pid": 4242,
                "created_at": created_at,
            });
            fs::write(
                info_path_for_socket(socket_path),
                serde_json::to_vec_pretty(&info).expect("encode session info"),
            )
            .expect("write session info");
            assert_eq!(socket_path.parent(), Some(info_dir));
        }

        fn acknowledge_one(
            listener: UnixListener,
            expected_type: &'static str,
        ) -> thread::JoinHandle<()> {
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept client");
                let mut request = String::new();
                BufReader::new(stream.try_clone().expect("clone stream"))
                    .read_line(&mut request)
                    .expect("read request");
                let value: Value = serde_json::from_str(&request).expect("decode request");
                assert_eq!(value["type"], expected_type);
                stream
                    .write_all(b"{\"ok\":true}\n")
                    .expect("acknowledge request");
            })
        }

        #[test]
        fn discovers_only_live_trusted_sessions() {
            let (runtime_dir, info_dir) = create_runtime();
            let live_path = info_dir.join("live.sock");
            let stale_path = info_dir.join("stale.sock");
            let listener = UnixListener::bind(&live_path).expect("bind live socket");
            write_info(&info_dir, &live_path, 20);
            write_info(&info_dir, &stale_path, 30);
            let server = acknowledge_one(listener, "ping");

            let sessions = list_pi_sessions_at(&runtime_dir).expect("list sessions");
            assert_eq!(sessions.len(), 1);
            assert_eq!(
                sessions[0].socket_path,
                live_path.to_string_lossy().to_string()
            );
            assert_eq!(sessions[0].cwd, "/work/paper");
            server.join().expect("join server");
            fs::remove_dir_all(runtime_dir).expect("remove test runtime");
        }

        #[test]
        fn sends_prompt_using_pi_nvim_protocol() {
            let (runtime_dir, info_dir) = create_runtime();
            let socket_path = info_dir.join("prompt.sock");
            let listener = UnixListener::bind(&socket_path).expect("bind prompt socket");
            write_info(&info_dir, &socket_path, 10);

            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept client");
                let mut request = String::new();
                BufReader::new(stream.try_clone().expect("clone stream"))
                    .read_line(&mut request)
                    .expect("read request");
                let value: Value = serde_json::from_str(&request).expect("decode request");
                assert_eq!(
                    value,
                    json!({ "type": "prompt", "message": "Explain this" })
                );
                stream
                    .write_all(b"{\"ok\":true}\n")
                    .expect("acknowledge request");
            });

            validate_runtime_dir(&runtime_dir).expect("validate runtime");
            validate_info_dir(&info_dir).expect("validate info directory");
            validate_session_socket(&info_dir, &socket_path).expect("validate session");
            send_request(
                &socket_path,
                &json!({ "type": "prompt", "message": "Explain this" }),
            )
            .expect("send prompt");

            server.join().expect("join server");
            fs::remove_dir_all(runtime_dir).expect("remove test runtime");
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::PiSession;

    pub fn list_pi_sessions() -> Result<Vec<PiSession>, String> {
        Err("The Pi bridge is supported only on Linux and macOS".to_string())
    }

    pub fn send_pi_prompt(_socket_path: &str, _message: &str) -> Result<(), String> {
        Err("The Pi bridge is supported only on Linux and macOS".to_string())
    }
}
