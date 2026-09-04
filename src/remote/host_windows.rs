//! Windows remote-host side of the SSH stdio bridge.

use interprocess::TryClone as _;
use std::io;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

const SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(crate) fn run_remote_client_bridge() -> io::Result<()> {
    ensure_remote_server_running()?;

    let socket_path = crate::server::socket_paths::client_socket_path();
    let stream = crate::ipc::connect_local_stream(&socket_path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "failed to connect to remote Herdr client socket {}: {err}",
                socket_path.display()
            ),
        )
    })?;

    let mut socket_to_stdout = stream.try_clone()?;
    let mut stdin_to_socket = stream;
    let upload_done = Arc::new(AtomicBool::new(false));
    let worker_done = Arc::clone(&upload_done);
    let _upload = thread::spawn(move || {
        let mut stdin = io::stdin();
        let result = io::copy(&mut stdin, &mut stdin_to_socket);
        worker_done.store(true, Ordering::Release);
        result
    });

    let mut stdout = io::stdout().lock();
    copy_socket_to_writer(&mut socket_to_stdout, &mut stdout, &upload_done).map(|_| ())
}

fn copy_socket_to_writer<W: io::Write>(
    stream: &mut crate::ipc::LocalStream,
    writer: &mut W,
    upload_done: &AtomicBool,
) -> io::Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0;
    loop {
        match crate::ipc::poll_local_stream_read_count(stream, &mut buffer)? {
            crate::ipc::LocalStreamReadCount::Data(read) => {
                writer.write_all(&buffer[..read])?;
                writer.flush()?;
                total += read as u64;
            }
            crate::ipc::LocalStreamReadCount::Pending => {
                if upload_done.load(Ordering::Acquire) {
                    return Ok(total);
                }
                thread::sleep(SOCKET_POLL_INTERVAL);
            }
            crate::ipc::LocalStreamReadCount::Closed => return Ok(total),
        }
    }
}

fn ensure_remote_server_running() -> io::Result<()> {
    let socket_path = crate::server::socket_paths::client_socket_path();
    if crate::server::autodetect::is_server_listening() {
        let status = crate::api::read_runtime_status_at(
            &crate::api::socket_path(),
            Duration::from_millis(500),
        )?
        .ok_or_else(|| io::Error::other("remote server status API is unavailable"))?;
        if status
            .capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.endpoint_protocol_generation)
            == Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION)
        {
            return Ok(());
        }
        return Err(io::Error::other(
            "remote herdr server needs one final update before this bridge can attach; rerun `herdr --remote` from an interactive terminal to approve it",
        ));
    }

    crate::server::autodetect::spawn_server_daemon()?;
    crate::server::autodetect::wait_for_server_socket(&socket_path, Duration::from_secs(5))
}

#[cfg(test)]
mod tests {
    use super::*;
    use interprocess::local_socket::traits::Listener as _;
    use std::io::Write as _;
    use std::path::PathBuf;

    fn test_socket_path() -> PathBuf {
        crate::platform::remote_private_temp_base().join(format!(
            "remote-host-bridge-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[test]
    fn windows_remote_bridge_preserves_server_bytes() {
        let path = test_socket_path();
        let _ = std::fs::remove_file(&path);
        crate::ipc::prepare_socket_path(&path, |_| "test socket busy".to_string()).unwrap();
        let listener = crate::ipc::bind_private_local_listener(&path).unwrap();
        let mut client = crate::ipc::connect_local_stream(&path).unwrap();
        let mut server = listener.accept().unwrap();
        let payload = b"\0remote\xffbytes\r\n";
        server.write_all(payload).unwrap();
        drop(server);

        let mut output = Vec::new();
        copy_socket_to_writer(&mut client, &mut output, &AtomicBool::new(false)).unwrap();

        assert_eq!(output, payload);
        drop(listener);
        let _ = std::fs::remove_file(path);
    }
}
