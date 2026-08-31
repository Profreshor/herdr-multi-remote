use std::io;
#[cfg(unix)]
use std::io::IsTerminal as _;
use std::time::Duration;

use interprocess::local_socket::traits::Stream as _;
#[cfg(windows)]
use tracing::debug;
use tracing::info;

use crate::ipc::LocalStream;
use crate::protocol::{
    self, ClientMessage, RenderEncoding, ServerMessage, MAX_FRAME_SIZE, PROTOCOL_VERSION,
};

use super::{shell, terminal_setup::is_ssh_session, ClientError};

/// Time to wait for the server's Welcome reply during the handshake.
///
/// A local client talks to an already-connected server, so 5s is plenty. The
/// remote bridge client (`herdr --remote`) sits behind a fresh per-attach ssh
/// connection whose cold-connect (TCP + key exchange + auth) happens inside this
/// window; on a high-latency link that easily exceeds 5s, so it gets a far
/// larger budget. See issue #753.
pub(super) const LOCAL_HANDSHAKE_READ_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const REMOTE_HANDSHAKE_READ_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) fn is_remote_client_process() -> bool {
    std::env::var(crate::remote::REMOTE_KEYBINDINGS_ENV_VAR).is_ok()
}

pub(super) fn client_shell_keybinding_source() -> shell::ClientShellKeybindingSource {
    match std::env::var(crate::remote::REMOTE_KEYBINDINGS_ENV_VAR)
        .ok()
        .as_deref()
    {
        Some("server") => shell::ClientShellKeybindingSource::Endpoint,
        Some(_) => shell::ClientShellKeybindingSource::RemoteLocal,
        None => shell::ClientShellKeybindingSource::Local,
    }
}

pub(super) fn handshake_read_timeout() -> Duration {
    if is_remote_client_process() {
        return REMOTE_HANDSHAKE_READ_TIMEOUT;
    }
    LOCAL_HANDSHAKE_READ_TIMEOUT
}

#[cfg(any(unix, test))]
pub(super) fn direct_graphics_profile_values(
    term_program: &str,
    term: &str,
    kitty_window: bool,
    blocked_transport: bool,
    terminals: bool,
) -> bool {
    let supported = term_program.eq_ignore_ascii_case("ghostty")
        || term_program.eq_ignore_ascii_case("wezterm")
        || matches!(term, "xterm-ghostty" | "xterm-kitty" | "xterm-wezterm")
        || kitty_window;
    supported && !blocked_transport && terminals
}

#[cfg(unix)]
fn direct_graphics_profile_allowed() -> bool {
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let term = std::env::var("TERM").unwrap_or_default();
    direct_graphics_profile_values(
        &term_program,
        &term,
        std::env::var_os("KITTY_WINDOW_ID").is_some(),
        is_remote_client_process()
            || is_ssh_session()
            || std::env::var_os("TMUX").is_some()
            || std::env::var_os("STY").is_some(),
        io::stdin().is_terminal() && io::stdout().is_terminal(),
    )
}

#[cfg(not(unix))]
fn direct_graphics_profile_allowed() -> bool {
    false
}

#[cfg(windows)]
fn set_handshake_recv_timeout(
    stream: &LocalStream,
    timeout: Option<Duration>,
    context: &'static str,
) -> Result<(), ClientError> {
    match stream.set_recv_timeout(timeout) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::Unsupported => {
            debug!(err = %err, context, "client socket receive timeout unavailable");
            Ok(())
        }
        Err(err) => Err(ClientError::ConnectionFailed(err)),
    }
}

#[cfg(not(windows))]
fn set_handshake_recv_timeout(
    stream: &LocalStream,
    timeout: Option<Duration>,
    _context: &'static str,
) -> Result<(), ClientError> {
    stream
        .set_recv_timeout(timeout)
        .map_err(ClientError::ConnectionFailed)
}

/// Performs the client→server handshake.
///
/// Sends TerminalHello (or ClientShellHello) with the terminal size and protocol
/// version, then reads the Welcome response.
pub(super) fn do_handshake(
    stream: &mut LocalStream,
    cols: u16,
    rows: u16,
    cell_width_px: u32,
    cell_height_px: u32,
    exact_cell_size: bool,
    shell_surface_size: Option<crate::protocol::ClientSurfaceSize>,
    endpoint_keybindings: bool,
    mouse_capture: bool,
) -> Result<RenderEncoding, ClientError> {
    stream
        .set_nonblocking(false)
        .map_err(ClientError::ConnectionFailed)?;

    let hello = if let Some(surface_size) = shell_surface_size {
        ClientMessage::ClientShellHello {
            version: PROTOCOL_VERSION,
            cell_width_px,
            cell_height_px,
            surface_size,
            pixel_mouse: exact_cell_size && cfg!(unix),
            direct_graphics: exact_cell_size
                && cell_width_px > 0
                && cell_height_px > 0
                && direct_graphics_profile_allowed(),
            endpoint_keybindings,
            mouse_capture,
        }
    } else {
        ClientMessage::TerminalHello {
            version: PROTOCOL_VERSION,
            cols,
            rows,
            cell_width_px,
            cell_height_px,
            pixel_mouse: exact_cell_size && cfg!(unix),
        }
    };
    protocol::write_message(stream, &hello)
        .map_err(|e| ClientError::ConnectionFailed(io::Error::other(e.to_string())))?;

    set_handshake_recv_timeout(
        stream,
        Some(handshake_read_timeout()),
        "client handshake read timeout unavailable",
    )?;
    let welcome: ServerMessage = protocol::read_message(stream, MAX_FRAME_SIZE)?;
    set_handshake_recv_timeout(
        stream,
        None,
        "failed to clear client handshake read timeout",
    )?;

    match welcome {
        ServerMessage::Welcome {
            version,
            encoding,
            error,
        } => {
            if let Some(error) = error {
                return Err(ClientError::HandshakeRejected { version, error });
            }
            info!(version, ?encoding, "handshake succeeded");
            Ok(encoding)
        }
        _ => Err(ClientError::Protocol(protocol::FramingError::Io(
            io::Error::new(io::ErrorKind::InvalidData, "expected Welcome message"),
        ))),
    }
}
