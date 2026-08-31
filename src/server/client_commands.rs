use std::io;
use std::sync::mpsc;

use tokio::sync::mpsc as tokio_mpsc;

use crate::api::schema::{ErrorBody, ErrorResponse, Method};

use super::client_transport::ServerEvent;

pub(crate) const MAX_ENDPOINT_COMMAND_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_ENDPOINT_BOOT_ID_BYTES: usize = 128;
pub(crate) const MAX_ENDPOINT_REQUEST_ID_BYTES: usize = 128;
const ENDPOINT_RESPONSE_CHUNK_BYTES: usize = 512 * 1024;

pub(crate) fn supports_client_shell_method(method: &Method) -> bool {
    matches!(
        method,
        Method::CommandInvoke(_)
            | Method::IntegrationInstall(_)
            | Method::IntegrationList(_)
            | Method::LayoutSetSplitRatio(_)
            | Method::PaneClose(_)
            | Method::PaneCopyMotion(_)
            | Method::PaneCopySearch(_)
            | Method::PaneEditScrollback(_)
            | Method::PaneFocus(_)
            | Method::PaneFocusDirection(_)
            | Method::PaneInputSet(_)
            | Method::PaneLinkActivate(_)
            | Method::PaneRename(_)
            | Method::PaneResize(_)
            | Method::PaneScroll(_)
            | Method::PaneSelectionRead(_)
            | Method::PaneSplit(_)
            | Method::PaneSwap(_)
            | Method::PaneZoom(_)
            | Method::ProductAnnouncementDismiss(_)
            | Method::ReleaseNotesDismiss(_)
            | Method::ServerReloadConfig(_)
            | Method::TabClose(_)
            | Method::TabCreate(_)
            | Method::TabFocus(_)
            | Method::TabMove(_)
            | Method::TabRename(_)
            | Method::WorkspaceClose(_)
            | Method::WorkspaceCreate(_)
            | Method::WorkspaceFocus(_)
            | Method::WorkspaceMove(_)
            | Method::WorkspaceMoveBlock(_)
            | Method::WorkspaceRename(_)
            | Method::WorktreeCreate(_)
            | Method::WorktreeList(_)
            | Method::WorktreeOpen(_)
            | Method::WorktreeRemove(_)
    )
}

pub(crate) fn error_response(id: String, code: &str, message: impl Into<String>) -> String {
    serde_json::to_string(&ErrorResponse {
        id,
        error: ErrorBody {
            code: code.into(),
            message: message.into(),
        },
    })
    .unwrap_or_else(|_| {
        r#"{"id":"","error":{"code":"serialization_error","message":"failed to serialize endpoint response"}}"#.into()
    })
}

pub(crate) fn error_message(
    boot_id: String,
    request_id: String,
    code: &str,
    message: impl Into<String>,
) -> crate::protocol::ServerMessage {
    let response = error_response(request_id.clone(), code, message);
    crate::protocol::ServerMessage::ClientShellEndpointResponseChunk {
        boot_id,
        request_id,
        final_chunk: true,
        data: response.into_bytes(),
    }
}

pub(crate) fn spawn_response_waiter(
    client_id: u64,
    boot_id: String,
    request_id: String,
    response_rx: mpsc::Receiver<String>,
    server_event_tx: tokio_mpsc::Sender<ServerEvent>,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name("herdr-client-endpoint-response".into())
        .spawn(move || {
            let response = response_rx.recv().unwrap_or_else(|_| {
                error_response(
                    request_id.clone(),
                    "server_unavailable",
                    "endpoint command ended without a response",
                )
            });
            let response = response.into_bytes();
            if response.is_empty() {
                let _ = server_event_tx.blocking_send(
                    ServerEvent::ClientShellEndpointResponseChunkReady {
                        client_id,
                        boot_id,
                        request_id,
                        final_chunk: true,
                        data: Vec::new(),
                    },
                );
                return;
            }
            let chunk_count = response.len().div_ceil(ENDPOINT_RESPONSE_CHUNK_BYTES);
            for (index, chunk) in response.chunks(ENDPOINT_RESPONSE_CHUNK_BYTES).enumerate() {
                if server_event_tx
                    .blocking_send(ServerEvent::ClientShellEndpointResponseChunkReady {
                        client_id,
                        boot_id: boot_id.clone(),
                        request_id: request_id.clone(),
                        final_chunk: index + 1 == chunk_count,
                        data: chunk.to_vec(),
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_shell_lane_excludes_api_front_door_and_lifecycle_methods() {
        assert!(supports_client_shell_method(&Method::ServerReloadConfig(
            crate::api::schema::EmptyParams::default(),
        )));
        assert!(supports_client_shell_method(&Method::PaneLinkActivate(
            crate::api::schema::PaneLinkActivateParams {
                pane_id: "w1:p1".into(),
                viewport_row: 0,
                col: 0,
                content_revision: None,
                offset_from_bottom: None,
            },
        )));
        assert!(!supports_client_shell_method(&Method::Ping(
            crate::api::schema::PingParams::default(),
        )));
        assert!(!supports_client_shell_method(&Method::ServerStop(
            crate::api::schema::EmptyParams::default(),
        )));
    }

    #[test]
    fn endpoint_responses_are_chunked_without_truncation() {
        let (response_tx, response_rx) = mpsc::channel();
        let (event_tx, mut event_rx) = tokio_mpsc::channel(8);
        spawn_response_waiter(
            7,
            "boot-a".into(),
            "request-a".into(),
            response_rx,
            event_tx,
        )
        .unwrap();
        let response = "x".repeat(ENDPOINT_RESPONSE_CHUNK_BYTES + 17);
        response_tx.send(response.clone()).unwrap();

        let mut received = Vec::new();
        loop {
            let ServerEvent::ClientShellEndpointResponseChunkReady {
                client_id,
                boot_id,
                request_id,
                final_chunk,
                data,
            } = event_rx.blocking_recv().expect("response chunk")
            else {
                panic!("expected response chunk");
            };
            assert_eq!(client_id, 7);
            assert_eq!(boot_id, "boot-a");
            assert_eq!(request_id, "request-a");
            received.extend(data);
            if final_chunk {
                break;
            }
        }

        assert_eq!(received, response.as_bytes());
    }
}
