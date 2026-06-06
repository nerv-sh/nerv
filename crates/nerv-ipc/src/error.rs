use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error(transparent)]
    Send(#[from] SendError),
    #[error(transparent)]
    Recv(#[from] RecvError),
    #[error("timeout")]
    Timeout,
    #[error(transparent)]
    Dir(#[from] nerv_util::directories::DirectoryError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[cfg(unix)]
    #[error(transparent)]
    Nix(#[from] nix::Error),
}

#[derive(Debug, Error)]
pub enum ConnectError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("timeout connecting to socket")]
    Timeout,
    #[error("invalid permissions on socket dir")]
    IncorrectSocketPermissions,
}

#[derive(Debug, Error)]
pub enum SendError {
    #[error(transparent)]
    Encode(#[from] nerv_proto::FigMessageEncodeError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Error)]
pub enum RecvError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Parse(#[from] nerv_proto::FigMessageParseError),
    #[error(transparent)]
    Decode(#[from] nerv_proto::FigMessageDecodeError),
    #[error("invalid message type")]
    InvalidMessageType,
}

impl RecvError {
    pub fn is_disconnect(&self) -> bool {
        if let RecvError::Io(io) = self {
            #[cfg(windows)]
            {
                // Windows error code
                let wsaeconnreset = 10054;
                if let Some(err) = io.raw_os_error() {
                    if err == wsaeconnreset {
                        return true;
                    }
                }
            }
            matches!(io.kind(), std::io::ErrorKind::ConnectionAborted)
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_disconnect() {
        assert!(
            !RecvError::Decode(nerv_proto::FigMessageDecodeError::NameNotValid(
                "test".to_string()
            ))
            .is_disconnect()
        );
        assert!(
            RecvError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "error"
            ))
            .is_disconnect()
        );
        assert!(
            !RecvError::Io(std::io::Error::new(std::io::ErrorKind::WouldBlock, "error"))
                .is_disconnect()
        );
    }

    /// `Error::Timeout` is the only variant without a transparent
    /// source. Lock its Display string — daemon doctor parses it.
    #[test]
    fn error_timeout_display_is_stable() {
        assert_eq!(Error::Timeout.to_string(), "timeout");
        assert_eq!(
            ConnectError::Timeout.to_string(),
            "timeout connecting to socket"
        );
    }

    /// `Error` aggregates the per-stage error variants via #[from] —
    /// verify each kind round-trips and Display forwards. Catches
    /// accidental swap of two #[from] sources at refactor time.
    #[test]
    fn error_from_conversions_forward() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let wrapped: Error = io.into();
        assert!(matches!(wrapped, Error::Io(_)));

        let conn: Error = ConnectError::IncorrectSocketPermissions.into();
        assert!(matches!(conn, Error::Connect(_)));
        assert!(conn.to_string().contains("invalid permissions"));

        let send: Error = SendError::Io(std::io::Error::other("oops")).into();
        assert!(matches!(send, Error::Send(_)));
        assert!(send.to_string().contains("oops"));
    }

    /// Variants that aren't IO + ConnectionAborted should never be
    /// classified as disconnects. Catches a refactor that broadens
    /// the match arm by accident.
    #[test]
    fn is_disconnect_rejects_parse_and_invalid_message() {
        let parse = RecvError::Parse(nerv_proto::FigMessageParseError::InvalidHeader(
            "expected".into(),
            "got".into(),
        ));
        assert!(!parse.is_disconnect());
        assert!(!RecvError::InvalidMessageType.is_disconnect());
    }
}
