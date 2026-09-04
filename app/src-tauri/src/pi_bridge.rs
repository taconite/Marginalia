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

pub fn subscribe_pi_messages<F>(
    subscription_id: &str,
    socket_path: &str,
    on_event: F,
) -> Result<(), String>
where
    F: FnMut(serde_json::Value) -> Result<(), String>,
{
    platform::subscribe_pi_messages(subscription_id, socket_path, on_event)
}

pub fn reserve_pi_subscription(subscription_id: &str) -> Result<(), String> {
    platform::reserve_pi_subscription(subscription_id)
}

pub fn unsubscribe_pi_messages(subscription_id: &str) -> Result<(), String> {
    platform::unsubscribe_pi_messages(subscription_id)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod platform {
    use super::PiSession;
    use serde::Deserialize;
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::fs::{self, Metadata};
    use std::io::{BufRead, BufReader, Read as _, Write};
    use std::net::Shutdown;
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    const MAX_INFO_BYTES: u64 = 64 * 1024;
    const MAX_STREAM_EVENT_BYTES: usize = 4 * 1024 * 1024;
    const SOCKET_TIMEOUT: Duration = Duration::from_secs(2);

    static SUBSCRIPTIONS: OnceLock<Mutex<HashMap<String, Option<UnixStream>>>> = OnceLock::new();

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

    pub fn subscribe_pi_messages<F>(
        subscription_id: &str,
        socket_path: &str,
        on_event: F,
    ) -> Result<(), String>
    where
        F: FnMut(Value) -> Result<(), String>,
    {
        if subscription_id.trim().is_empty() {
            return Err("Subscription ID cannot be empty".to_string());
        }

        let result = (|| {
            let runtime_dir = runtime_dir();
            validate_runtime_dir(&runtime_dir)?;
            let info_dir = runtime_dir.join("sockets");
            validate_info_dir(&info_dir)?;

            let socket_path = PathBuf::from(socket_path);
            validate_session_socket(&info_dir, &socket_path)?;
            subscribe_request(subscription_id, &socket_path, on_event)
        })();
        if result.is_err() {
            let _ = unsubscribe_pi_messages(subscription_id);
        }
        result
    }

    pub fn reserve_pi_subscription(subscription_id: &str) -> Result<(), String> {
        if subscription_id.trim().is_empty() {
            return Err("Subscription ID cannot be empty".to_string());
        }
        let mut subscriptions = subscriptions()
            .lock()
            .map_err(|_| "Pi subscription registry is unavailable".to_string())?;
        if subscriptions.contains_key(subscription_id) {
            return Err("Pi subscription already exists".to_string());
        }
        subscriptions.insert(subscription_id.to_string(), None);
        Ok(())
    }

    pub fn unsubscribe_pi_messages(subscription_id: &str) -> Result<(), String> {
        let stream = subscriptions()
            .lock()
            .map_err(|_| "Pi subscription registry is unavailable".to_string())?
            .remove(subscription_id)
            .flatten();
        if let Some(stream) = stream {
            stream
                .shutdown(Shutdown::Both)
                .map_err(|error| format!("Cannot close Pi subscription: {error}"))?;
        }
        Ok(())
    }

    fn subscriptions() -> &'static Mutex<HashMap<String, Option<UnixStream>>> {
        SUBSCRIPTIONS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn subscribe_request<F>(
        subscription_id: &str,
        socket_path: &Path,
        mut on_event: F,
    ) -> Result<(), String>
    where
        F: FnMut(Value) -> Result<(), String>,
    {
        if !subscription_exists(subscription_id)? {
            return Ok(());
        }

        let result = subscribe_request_inner(subscription_id, socket_path, &mut on_event);
        let _ = unsubscribe_pi_messages(subscription_id);
        result
    }

    fn subscribe_request_inner<F>(
        subscription_id: &str,
        socket_path: &Path,
        on_event: &mut F,
    ) -> Result<(), String>
    where
        F: FnMut(Value) -> Result<(), String>,
    {
        let mut stream = UnixStream::connect(socket_path)
            .map_err(|error| format!("Cannot connect to Pi: {error}"))?;
        stream
            .set_read_timeout(Some(SOCKET_TIMEOUT))
            .map_err(|error| format!("Cannot configure Pi socket: {error}"))?;
        stream
            .set_write_timeout(Some(SOCKET_TIMEOUT))
            .map_err(|error| format!("Cannot configure Pi socket: {error}"))?;

        serde_json::to_writer(&mut stream, &json!({ "type": "subscribe" }))
            .map_err(|error| format!("Cannot encode Pi request: {error}"))?;
        stream
            .write_all(b"\n")
            .map_err(|error| format!("Cannot send request to Pi: {error}"))?;
        stream
            .flush()
            .map_err(|error| format!("Cannot flush request to Pi: {error}"))?;

        let cancel_stream = stream
            .try_clone()
            .map_err(|error| format!("Cannot prepare Pi subscription: {error}"))?;
        {
            let mut subscriptions = subscriptions()
                .lock()
                .map_err(|_| "Pi subscription registry is unavailable".to_string())?;
            let Some(slot) = subscriptions.get_mut(subscription_id) else {
                return Ok(());
            };
            *slot = Some(cancel_stream);
        }

        let mut reader = BufReader::new(stream);
        let mut response_line = String::new();
        let mut acknowledged = false;
        loop {
            response_line.clear();
            let bytes = match reader
                .by_ref()
                .take((MAX_STREAM_EVENT_BYTES + 1) as u64)
                .read_line(&mut response_line)
            {
                Ok(bytes) => bytes,
                Err(_) if !subscription_exists(subscription_id)? => return Ok(()),
                Err(error) => return Err(format!("Cannot read Pi response: {error}")),
            };
            if bytes == 0 {
                if !subscription_exists(subscription_id)? {
                    return Ok(());
                }
                return Err("Pi closed the response stream unexpectedly".to_string());
            }
            if response_line.len() > MAX_STREAM_EVENT_BYTES {
                return Err("Pi response event is too large".to_string());
            }

            let event: Value = serde_json::from_str(&response_line)
                .map_err(|error| format!("Pi returned invalid JSON: {error}"))?;
            if !acknowledged {
                let response: SocketResponse = serde_json::from_value(event)
                    .map_err(|error| format!("Pi returned an invalid acknowledgement: {error}"))?;
                if !response.ok {
                    return Err(response
                        .error
                        .unwrap_or_else(|| "Pi rejected the request".to_string()));
                }
                acknowledged = true;
                reader
                    .get_ref()
                    .set_read_timeout(None)
                    .map_err(|error| format!("Cannot configure Pi subscription: {error}"))?;
                on_event(json!({ "type": "subscribed" }))?;
                continue;
            }

            match event.get("type").and_then(Value::as_str) {
                Some("assistant") => on_event(event)?,
                Some("closed") => return Ok(()),
                Some("error") => {
                    return Err(event
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("Pi could not complete the request")
                        .to_string());
                }
                _ => return Err("Pi returned an unsupported response event".to_string()),
            }
        }
    }

    fn subscription_exists(subscription_id: &str) -> Result<bool, String> {
        Ok(subscriptions()
            .lock()
            .map_err(|_| "Pi subscription registry is unavailable".to_string())?
            .contains_key(subscription_id))
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
        use std::io::Read;
        use std::os::unix::net::UnixListener;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::mpsc;
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

        #[test]
        fn subscribes_to_assistant_messages_until_the_session_closes() {
            let socket_path = test_runtime_dir().join("stream.sock");
            fs::create_dir_all(socket_path.parent().expect("stream parent"))
                .expect("create stream parent");
            let listener = UnixListener::bind(&socket_path).expect("bind stream socket");
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept client");
                let mut request = String::new();
                BufReader::new(stream.try_clone().expect("clone stream"))
                    .read_line(&mut request)
                    .expect("read request");
                let value: Value = serde_json::from_str(&request).expect("decode request");
                assert_eq!(value, json!({ "type": "subscribe" }));
                stream
                    .write_all(
                        b"{\"ok\":true,\"type\":\"subscribed\"}\n\
                          {\"type\":\"assistant\",\"content\":\"The answer\",\"timestamp\":42}\n\
                          {\"type\":\"closed\"}\n",
                    )
                    .expect("write subscription events");
            });

            let mut events = Vec::new();
            reserve_pi_subscription("test-subscription").expect("reserve subscription");
            subscribe_request("test-subscription", &socket_path, |event| {
                events.push(event);
                Ok(())
            })
            .expect("subscribe to messages");

            assert_eq!(
                events,
                vec![
                    json!({ "type": "subscribed" }),
                    json!({
                        "type": "assistant",
                        "content": "The answer",
                        "timestamp": 42,
                    }),
                ]
            );
            server.join().expect("join server");
            fs::remove_dir_all(socket_path.parent().expect("stream parent"))
                .expect("remove stream runtime");
        }

        #[test]
        fn unsubscribe_closes_an_active_subscription() {
            let socket_path = test_runtime_dir().join("unsubscribe.sock");
            fs::create_dir_all(socket_path.parent().expect("subscription parent"))
                .expect("create subscription parent");
            let listener = UnixListener::bind(&socket_path).expect("bind subscription socket");
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept client");
                let mut request = String::new();
                BufReader::new(stream.try_clone().expect("clone stream"))
                    .read_line(&mut request)
                    .expect("read request");
                stream
                    .write_all(b"{\"ok\":true,\"type\":\"subscribed\"}\n")
                    .expect("acknowledge subscription");
                let mut byte = [0];
                assert_eq!(stream.read(&mut byte).expect("wait for disconnect"), 0);
            });

            let client_path = socket_path.clone();
            let (ready_tx, ready_rx) = mpsc::channel();
            let client = thread::spawn(move || {
                reserve_pi_subscription("cancel-subscription").expect("reserve subscription");
                subscribe_request("cancel-subscription", &client_path, |event| {
                    if event["type"] == "subscribed" {
                        ready_tx.send(()).expect("signal subscription readiness");
                    }
                    Ok(())
                })
            });

            ready_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("subscription became ready");
            unsubscribe_pi_messages("cancel-subscription").expect("unsubscribe");
            client
                .join()
                .expect("join subscription client")
                .expect("subscription stopped cleanly");
            server.join().expect("join server");
            fs::remove_dir_all(socket_path.parent().expect("subscription parent"))
                .expect("remove subscription runtime");
        }

        #[test]
        fn rejects_an_oversized_subscription_event_while_reading() {
            let socket_path = test_runtime_dir().join("oversized-stream.sock");
            fs::create_dir_all(socket_path.parent().expect("stream parent"))
                .expect("create stream parent");
            let listener = UnixListener::bind(&socket_path).expect("bind stream socket");
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept client");
                let mut request = String::new();
                BufReader::new(stream.try_clone().expect("clone stream"))
                    .read_line(&mut request)
                    .expect("read request");
                stream
                    .write_all(b"{\"ok\":true,\"type\":\"subscribed\"}\n")
                    .expect("acknowledge subscription");
                let oversized = vec![b'x'; MAX_STREAM_EVENT_BYTES + 1];
                let _ = stream.write_all(&oversized);
            });

            reserve_pi_subscription("oversized-subscription").expect("reserve subscription");
            let error = subscribe_request("oversized-subscription", &socket_path, |_| Ok(()))
                .expect_err("reject oversized event");

            assert_eq!(error, "Pi response event is too large");
            server.join().expect("join server");
            fs::remove_dir_all(socket_path.parent().expect("stream parent"))
                .expect("remove stream runtime");
        }

        #[test]
        fn unsubscribe_before_the_worker_starts_cancels_the_subscription() {
            reserve_pi_subscription("cancel-before-worker").expect("reserve subscription");
            unsubscribe_pi_messages("cancel-before-worker").expect("cancel subscription");

            subscribe_request(
                "cancel-before-worker",
                Path::new("/unreachable/pi.sock"),
                |_| Ok(()),
            )
            .expect("skip cancelled subscription");
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::PiSession;
    use serde_json::Value;

    pub fn list_pi_sessions() -> Result<Vec<PiSession>, String> {
        Err("The Pi bridge is supported only on Linux and macOS".to_string())
    }

    pub fn send_pi_prompt(_socket_path: &str, _message: &str) -> Result<(), String> {
        Err("The Pi bridge is supported only on Linux and macOS".to_string())
    }

    pub fn subscribe_pi_messages<F>(
        _subscription_id: &str,
        _socket_path: &str,
        _on_event: F,
    ) -> Result<(), String>
    where
        F: FnMut(Value) -> Result<(), String>,
    {
        Err("The Pi bridge is supported only on Linux and macOS".to_string())
    }

    pub fn reserve_pi_subscription(_subscription_id: &str) -> Result<(), String> {
        Err("The Pi bridge is supported only on Linux and macOS".to_string())
    }

    pub fn unsubscribe_pi_messages(_subscription_id: &str) -> Result<(), String> {
        Err("The Pi bridge is supported only on Linux and macOS".to_string())
    }
}
