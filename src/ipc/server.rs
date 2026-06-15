//! Unix socket server run by the TUI to answer now-playing requests.
//!
//! Binds the socket (unlinking only stale files first), then answers each
//! one-shot connection from a cached [`PlaybackSnapshot`] watched over a
//! `watch` channel. Serving from a cached snapshot keeps responses instant and
//! avoids touching the player from the IPC path.

use crate::error::IpcError;
use crate::ipc::NowPlayingPayload;
use crate::model::PlaybackSnapshot;
use std::io::ErrorKind;
use std::os::unix::fs::FileTypeExt as _;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;
use tokio::io::AsyncWriteExt as _;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

/// Bind the now-playing socket, removing only stale files at the path first.
///
/// # Errors
///
/// Returns [`IpcError::Socket`] if the parent directory cannot be created or
/// the socket cannot be bound.
pub fn bind() -> Result<UnixListener, IpcError> {
    let path = crate::ipc::socket_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| IpcError::Socket(format!("create socket dir: {e}")))?;
    }
    prepare_socket_path(&path)?;
    UnixListener::bind(&path).map_err(|e| IpcError::Socket(format!("bind socket: {e}")))
}

/// Ensure `path` can be bound without stealing a live server's socket.
fn prepare_socket_path(path: &Path) -> Result<(), IpcError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(IpcError::Socket(format!("stat socket path: {err}"))),
    };

    if metadata.file_type().is_socket() {
        return prepare_existing_socket(path);
    }
    std::fs::remove_file(path)
        .map_err(|err| IpcError::Socket(format!("unlink stale socket path: {err}")))
}

/// Probe an existing socket path; live servers are left alone, stale sockets are unlinked.
fn prepare_existing_socket(path: &Path) -> Result<(), IpcError> {
    match StdUnixStream::connect(path) {
        Ok(_stream) => Err(IpcError::Socket(format!(
            "now-playing socket already in use: {}",
            path.display()
        ))),
        Err(err) if err.kind() == ErrorKind::ConnectionRefused => std::fs::remove_file(path)
            .map_err(|err| IpcError::Socket(format!("unlink stale socket: {err}"))),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(IpcError::Socket(format!("probe existing socket: {err}"))),
    }
}

/// Serve now-playing requests until the process exits.
///
/// Each accepted connection is answered with the current snapshot from
/// `snapshot`, serialized as a single JSON line, then closed. A per-connection
/// failure (write error, serialization error) is logged and skipped; it never
/// tears down the listener, so a crashed `now-playing` client cannot take the
/// TUI's IPC down with it.
///
/// # Errors
///
/// Returns [`IpcError::Socket`] when accepting a connection fails fatally
/// (the listener itself is broken, not merely one peer).
pub async fn serve(
    listener: UnixListener,
    snapshot: watch::Receiver<PlaybackSnapshot>,
) -> Result<(), IpcError> {
    loop {
        let (stream, _addr) = listener
            .accept()
            .await
            .map_err(|e| IpcError::Socket(format!("accept connection: {e}")))?;
        let payload = NowPlayingPayload::from(&*snapshot.borrow());
        if let Err(err) = handle_conn(stream, &payload).await {
            tracing::warn!(error = %err, "now-playing connection failed");
        }
    }
}

/// Write one snapshot payload to a connected client, then close.
///
/// The payload is serialized to JSON and flushed; closing the stream signals
/// end-of-input to the client's `read_to_end`.
async fn handle_conn(mut stream: UnixStream, payload: &NowPlayingPayload) -> Result<(), IpcError> {
    let body = serde_json::to_vec(payload)
        .map_err(|e| IpcError::Payload(format!("serialize now-playing payload: {e}")))?;
    stream
        .write_all(&body)
        .await
        .map_err(|e| IpcError::Socket(format!("write payload: {e}")))?;
    stream
        .shutdown()
        .await
        .map_err(|e| IpcError::Socket(format!("close connection: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::ipc::NowPlayingPayload;
    use crate::ipc::server::{handle_conn, prepare_socket_path, serve};
    use crate::model::{PlaybackSnapshot, PlaybackState};
    use tokio::io::AsyncReadExt as _;
    use tokio::net::{UnixListener, UnixStream};
    use tokio::sync::watch;

    fn snapshot(track: &str, artist: &str) -> PlaybackSnapshot {
        PlaybackSnapshot {
            track: Some(track.to_owned()),
            artist: Some(artist.to_owned()),
            state: PlaybackState::Playing,
            position_ms: 1_000,
            duration_ms: 200_000,
            volume: 50,
        }
    }

    #[tokio::test]
    async fn handle_conn_writes_json_payload() {
        let (client, server) = UnixStream::pair().expect("socketpair");
        let payload = NowPlayingPayload::from(&snapshot("Title", "Artist"));
        let expected = payload.clone();

        let writer = tokio::spawn(async move { handle_conn(server, &payload).await });

        let mut client = client;
        let mut buf = Vec::new();
        client.read_to_end(&mut buf).await.expect("read");
        writer.await.expect("join").expect("handle_conn");

        let decoded: NowPlayingPayload = serde_json::from_slice(&buf).expect("decode");
        assert_eq!(decoded, expected);
    }

    #[tokio::test]
    async fn serve_answers_each_connection_from_the_watched_snapshot() {
        let dir = std::env::temp_dir().join(format!("spot-defy-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("serve.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind");

        let (tx, rx) = watch::channel(snapshot("First", "A"));
        let server = tokio::spawn(serve(listener, rx));

        let first = read_payload(&path).await;
        assert_eq!(first.track.as_deref(), Some("First"));

        tx.send(snapshot("Second", "B")).expect("update snapshot");
        let second = read_payload(&path).await;
        assert_eq!(second.track.as_deref(), Some("Second"));

        server.abort();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn prepare_socket_path_refuses_live_socket() {
        let path = test_socket_path("sdf-live");
        let _ = std::fs::remove_file(&path);
        let _listener = std::os::unix::net::UnixListener::bind(&path).expect("bind live socket");

        let err = prepare_socket_path(&path).expect_err("live socket must be refused");

        assert!(err.to_string().contains("already in use"));
        std::os::unix::net::UnixStream::connect(&path).expect("live socket still exists");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn prepare_socket_path_unlinks_stale_socket() {
        let path = test_socket_path("sdf-stale");
        let _ = std::fs::remove_file(&path);
        {
            let _listener =
                std::os::unix::net::UnixListener::bind(&path).expect("bind stale socket");
        }

        prepare_socket_path(&path).expect("stale socket should be removed");

        assert!(!path.exists());
    }

    async fn read_payload(path: &std::path::Path) -> NowPlayingPayload {
        let mut stream = UnixStream::connect(path).await.expect("connect");
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.expect("read");
        serde_json::from_slice(&buf).expect("decode")
    }

    fn test_socket_path(prefix: &str) -> std::path::PathBuf {
        std::path::PathBuf::from("/tmp").join(format!(
            "{prefix}-{}-{}.sock",
            std::process::id(),
            unique_test_suffix()
        ))
    }

    fn unique_test_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos() % 1_000_000_000)
    }
}
