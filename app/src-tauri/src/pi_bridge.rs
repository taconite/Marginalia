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
    ssh::cancel(subscription_id);
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

    pub(super) fn subscribe_request<F>(
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

    pub(super) fn send_request(socket_path: &Path, request: &Value) -> Result<(), String> {
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
            PathBuf::from("/tmp").join(format!(
                "marginalia-pi-{}-{timestamp}-{}",
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
                    json!({ "type": "prompt", "message": "中".repeat(28_000) })
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
                &json!({ "type": "prompt", "message": "中".repeat(28_000) }),
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

pub mod ssh {
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
    #[cfg(unix)]
    use std::os::unix::{fs::DirBuilderExt, process::CommandExt};
    use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
    use std::sync::{Arc, Mutex, OnceLock};

    #[derive(Default)]
    struct State {
        cancelled: bool,
        child: Option<Child>,
    }
    type Jobs = Mutex<HashMap<String, Arc<Mutex<State>>>>;
    static JOBS: OnceLock<Jobs> = OnceLock::new();
    pub struct Job(String, Arc<Mutex<State>>);
    fn jobs() -> &'static Jobs {
        JOBS.get_or_init(Default::default)
    }
    pub fn reserve(id: &str) -> Result<Job, String> {
        let mut jobs = jobs().lock().map_err(|e| e.to_string())?;
        if id.is_empty() || jobs.contains_key(id) {
            return Err("Invalid or duplicate SSH operation ID".into());
        }
        let state = Arc::new(Mutex::new(State::default()));
        jobs.insert(id.into(), state.clone());
        Ok(Job(id.into(), state))
    }
    pub fn cancel(id: &str) {
        if let Some(state) = jobs().lock().unwrap().get(id) {
            let mut state = state.lock().unwrap();
            state.cancelled = true;
            if let Some(child) = state.child.as_mut() {
                stop(child);
            }
        }
    }
    fn stop(child: &mut Child) {
        #[cfg(unix)]
        // SAFETY: the child owns this process group; no pointers are used.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
    }
    impl Drop for Job {
        fn drop(&mut self) {
            cancel(&self.0);
            jobs().lock().unwrap().remove(&self.0);
            if let Some(mut child) = self.1.lock().unwrap().child.take() {
                let _ = child.wait();
            }
        }
    }
    fn command(host: &str, forwarding: Option<&str>, script: &str) -> Result<Command, String> {
        if !host.starts_with(|c: char| c.is_ascii_alphanumeric())
            || !host
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        {
            return Err("Invalid SSH host alias".into());
        }
        let mut command = Command::new("ssh");
        #[cfg(unix)]
        command.process_group(0);
        command.args([
            "-T",
            "-oBatchMode=yes",
            "-oConnectTimeout=10",
            "-oServerAliveInterval=15",
            "-oServerAliveCountMax=2",
            "-oControlPath=none",
            "-oControlMaster=no",
            "-oExitOnForwardFailure=yes",
        ]);
        if let Some(forwarding) = forwarding {
            command.args(["-L", forwarding]);
        }
        // Quote backslashes outside single quotes for both fish and POSIX shells.
        let script: String = script
            .chars()
            .map(|c| match c {
                '\'' => "'\\''".into(),
                '\\' => "'\\\\'".into(),
                _ => c.to_string(),
            })
            .collect();
        command.args(["--", host, &format!("sh -c '{script}'")]);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(command)
    }
    impl Job {
        fn directory(&self) -> std::path::PathBuf {
            let hash = format!("{:x}", Sha256::digest(self.0.as_bytes()));
            format!("/tmp/marginalia-{}-{}", std::process::id(), &hash[..16]).into()
        }
        fn execute<T>(
            &self,
            mut command: Command,
            tunnel: bool,
            work: impl FnOnce(ChildStdin, BufReader<ChildStdout>) -> Result<T, String>,
        ) -> Result<T, String> {
            let (input, output, error) = {
                let mut state = self.1.lock().unwrap();
                if state.cancelled {
                    return Err("SSH operation cancelled".into());
                }
                let mut child = command
                    .spawn()
                    .map_err(|e| format!("Cannot start SSH: {e}"))?;
                let pipes = (
                    child.stdin.take().unwrap(),
                    child.stdout.take().unwrap(),
                    child.stderr.take().unwrap(),
                );
                state.child = Some(child);
                pipes
            };
            let errors = std::thread::spawn(move || {
                let mut text = String::new();
                let _ = error.take(8192).read_to_string(&mut text);
                text
            });
            let result = work(input, BufReader::new(output));
            if tunnel || result.is_err() {
                if let Some(child) = self.1.lock().unwrap().child.as_mut() {
                    stop(child);
                }
            }
            let status = self
                .1
                .lock()
                .unwrap()
                .child
                .as_mut()
                .unwrap()
                .wait()
                .map_err(|e| e.to_string())?;
            let stderr = errors.join().unwrap_or_default();
            if !tunnel && !status.success() {
                return Err(format!("SSH failed: {}", stderr.trim()));
            }
            result.map_err(|error| format!("{error}. {}", stderr.trim()))
        }
        fn capture(
            &self,
            host: &str,
            script: &str,
            source: &mut impl Read,
        ) -> Result<String, String> {
            self.execute(command(host, None, script)?, false, |mut input, output| {
                std::io::copy(source, &mut input).map_err(|e| e.to_string())?;
                drop(input);
                let mut text = String::new();
                output
                    .take(4 * 1024 * 1024 + 1)
                    .read_to_string(&mut text)
                    .map_err(|e| e.to_string())?;
                if text.len() > 4 * 1024 * 1024 {
                    return Err("SSH output too large".into());
                }
                Ok(text)
            })
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        fn relay(
            &self,
            host: &str,
            socket: &str,
            request: &Value,
            on_event: impl FnMut(Value) -> Result<(), String>,
        ) -> Result<(), String> {
            if !socket.starts_with('/') || socket.contains([':', '\n', '\r']) {
                return Err("Invalid remote socket path".into());
            }
            let path = self.directory().join("pi.sock");
            let forwarding = format!("{}:{socket}", path.display());
            let command = command(host, Some(&forwarding), "printf 'ready\\n'; cat >/dev/null")?;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(self.directory())
                .map_err(|e| e.to_string())?;
            let result = self.execute(command, true, |_input, mut output| {
                let mut ready = String::new();
                output.read_line(&mut ready).map_err(|e| e.to_string())?;
                if ready != "ready\n" {
                    return Err("SSH tunnel failed to open".into());
                }
                if request["type"] == "subscribe" {
                    super::platform::reserve_pi_subscription(&self.0)?;
                    super::platform::subscribe_request(&self.0, &path, on_event)
                } else {
                    super::platform::send_request(&path, request)
                }
            });
            let _ = std::fs::remove_dir_all(self.directory());
            result
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        fn relay(
            &self,
            _: &str,
            _: &str,
            _: &Value,
            _: impl FnMut(Value) -> Result<(), String>,
        ) -> Result<(), String> {
            Err("SSH socket forwarding requires Linux or macOS".into())
        }
        pub fn run(
            self,
            host: &str,
            request: Value,
            on_event: impl FnMut(Value) -> Result<(), String>,
        ) -> Result<Value, String> {
            match request["op"].as_str() {
                Some("relay") => {
                    self.relay(
                        host,
                        request["socketPath"].as_str().ok_or("Missing Pi socket")?,
                        &request["request"],
                        on_event,
                    )?;
                    Ok(json!({"ok":true}))
                }
                Some("discover") => {
                    let metadata = self.capture(
                        host,
                        r#"set -eu
root="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/pi-nvim"
[ -d "$root" ] || root="${TMPDIR:-/tmp}/pi-nvim-$(id -u)"
[ -d "$root" ] || exit 0
[ "$(stat -c '%u:%a' "$root")" = "$(id -u):700" ]
for file in "$root"/sockets/*.sock.info; do
  [ -f "$file" ] && [ ! -L "$file" ] && [ -O "$file" ] && [ -S "${file%.info}" ] || continue
  [ "$(stat -c %s "$file")" -le 65536 ] || continue
  printf '\000'
  cat -- "$file" || continue
done"#,
                        &mut std::io::empty(),
                    )?;
                    let mut sessions = Vec::new();
                    for info in metadata
                        .split('\0')
                        .filter_map(|entry| serde_json::from_str::<Value>(entry).ok())
                    {
                        let Some(socket) = info["socket_path"].as_str() else {
                            continue;
                        };
                        if self
                            .relay(host, socket, &json!({"type":"ping"}), |_| Ok(()))
                            .is_ok()
                        {
                            sessions.push(json!({"socketPath":socket,"cwd":info["cwd"],"pid":info["pid"],"createdAt":info["created_at"]}));
                        }
                    }
                    sessions.sort_by_key(|info| std::cmp::Reverse(info["createdAt"].as_u64()));
                    Ok(json!(sessions))
                }
                Some("receive") => {
                    let mut file =
                        std::fs::File::open(request["path"].as_str().ok_or("Missing PDF path")?)
                            .map_err(|e| e.to_string())?;
                    let mut hash = Sha256::new();
                    let mut buffer = [0; 65536];
                    loop {
                        if self.1.lock().unwrap().cancelled {
                            return Err("PDF preparation cancelled".into());
                        }
                        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
                        if count == 0 {
                            break;
                        }
                        hash.update(&buffer[..count]);
                    }
                    let digest = format!("{:x}", hash.finalize());
                    let setup = format!("set -eu; dir=\"$HOME/.cache/marginalia/pdfs\"; target=\"$dir/{digest}.pdf\";");
                    let verify = format!(
                        "[ \"$(sha256sum < \"$target\" | cut -d ' ' -f 1)\" = '{digest}' ]"
                    );
                    let cached = self.capture(host, &format!("{setup} if [ -f \"$target\" ] && {verify}; then printf '%s' \"$target\"; fi"), &mut std::io::empty())?;
                    if !cached.is_empty() {
                        return Ok(json!({"path":cached}));
                    }
                    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
                    let script = format!(
                        r#"{setup}
mkdir -p -m 700 -- "$dir"
temporary=$(mktemp "$dir/.upload-XXXXXXXX")
trap 'rm -f -- "$temporary"' EXIT HUP INT TERM
cat > "$temporary"
[ "$(sha256sum < "$temporary" | cut -d ' ' -f 1)" = '{digest}' ]
mv -f -- "$temporary" "$target"
printf '%s' "$target""#
                    );
                    let path = self.capture(host, &script, &mut file)?;
                    if path.is_empty() {
                        return Err("Missing remote PDF path".into());
                    }
                    Ok(json!({"path":path}))
                }
                _ => Err("Unsupported SSH operation".into()),
            }
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn rejects_host_options_and_shell_syntax() {
            for host in ["", "-oProxyCommand=x", "host;x", "user@host", "host\n"] {
                assert!(command(host, None, "true").is_err());
            }
            let command = command(
                "ubuntu-work",
                Some("/tmp/local.sock:/run/remote.sock"),
                "true",
            )
            .unwrap();
            let args: Vec<_> = command.get_args().collect();
            assert!(args.iter().any(|arg| *arg == "-oBatchMode=yes"));
            assert!(args
                .windows(2)
                .any(|args| args == ["-L", "/tmp/local.sock:/run/remote.sock"]));
            assert!(args.iter().any(|arg| *arg == "ubuntu-work"));
        }
        #[test]
        fn cancellation_before_start_prevents_spawn() {
            let job = reserve("cancel-ssh-before-start").unwrap();
            cancel("cancel-ssh-before-start");
            assert!(job
                .run("example", json!({"op":"discover"}), |_| Ok(()))
                .unwrap_err()
                .contains("cancelled"));
        }
        #[test]
        fn cancellation_stops_and_reaps_a_child() {
            let job = reserve("cancel-running-ssh").unwrap();
            let child = Command::new("sleep").arg("60").spawn().unwrap();
            job.1.lock().unwrap().child = Some(child);
            cancel("cancel-running-ssh");
            let mut state = job.1.lock().unwrap();
            let status = state.child.as_mut().unwrap().wait().unwrap();
            assert!(!status.success());
            drop(state);
            drop(job);
            assert!(!jobs().lock().unwrap().contains_key("cancel-running-ssh"));
        }
    }
}
