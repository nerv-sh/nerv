use async_trait::async_trait;
use nerv_proto::FigProtobufEncodable;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tracing::{error, trace};

use crate::SendError;

#[async_trait]
pub trait SendMessage {
    async fn send_message<M>(&mut self, message: M) -> Result<(), SendError>
    where
        M: FigProtobufEncodable;
}

#[async_trait]
impl<T> SendMessage for T
where
    T: AsyncWrite + Unpin + Send,
{
    async fn send_message<M>(&mut self, message: M) -> Result<(), SendError>
    where
        M: FigProtobufEncodable,
    {
        let encoded_message = match message.encode_nerv_protobuf() {
            Ok(encoded_message) => encoded_message,
            Err(err) => {
                error!(%err, "Failed to encode message");
                return Err(err.into());
            }
        };

        self.write_all(&encoded_message).await?;
        self.flush().await?;

        trace!(?message, "Sent message");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use super::*;

    #[derive(Debug)]
    struct FailingMessage;

    impl nerv_proto::FigProtobufEncodable for FailingMessage {
        fn encode_nerv_protobuf(&self) -> Result<bytes::Bytes, nerv_proto::FigMessageEncodeError> {
            Err(std::io::Error::other("boom").into())
        }
    }

    struct FailingWriter;

    impl AsyncWrite for FailingWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "boom",
            )))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn test_message() -> nerv_proto::local::LocalMessage {
        nerv_proto::hooks::hook_to_message(nerv_proto::hooks::new_hide_hook())
    }

    #[tokio::test]
    async fn send_propagates_encode_error() {
        let mut buf: Vec<u8> = Vec::new();
        let err = buf.send_message(FailingMessage).await.unwrap_err();
        assert!(matches!(err, SendError::Encode(_)), "{err:?}");
    }

    #[tokio::test]
    async fn send_propagates_write_error() {
        let mut writer = FailingWriter;
        let err = writer.send_message(test_message()).await.unwrap_err();
        assert!(matches!(err, SendError::Io(_)), "{err:?}");
    }
}
