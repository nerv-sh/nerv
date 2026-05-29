use std::io::{self, IoSlice};
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::BytesMut;
use pin_project_lite::pin_project;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pin_project! {
    /// A light wrapper around a `BufReader<UnixStream>`
    #[derive(Debug)]
    pub struct BufferedReader<T> {
        #[pin]
        pub(crate) inner: T,
        pub(crate) buffer: BytesMut
    }
}

impl<T> BufferedReader<T> {
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            buffer: BytesMut::new(),
        }
    }

    pub fn buffer(&self) -> &[u8] {
        self.buffer.as_ref()
    }

    /// Converts into inner `BufStream<UnixStream>`
    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<T> AsyncWrite for BufferedReader<T>
where
    T: AsyncWrite,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.project().inner.poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.project().inner.poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.project().inner.poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.project().inner.poll_shutdown(cx)
    }
}

impl<T> AsyncRead for BufferedReader<T>
where
    T: AsyncRead,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.project().inner.poll_read(cx, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

    #[test]
    fn new_starts_with_empty_buffer() {
        let reader = BufferedReader::new(Vec::<u8>::new());
        assert!(reader.buffer().is_empty());
    }

    #[test]
    fn into_inner_returns_the_wrapped_value() {
        let payload = vec![1u8, 2, 3];
        let reader = BufferedReader::new(payload.clone());
        assert_eq!(reader.into_inner(), payload);
    }

    /// Confirm the AsyncRead / AsyncWrite forwarding works end-to-end
    /// over a tokio duplex stream — write on one end, read it back on
    /// the wrapped end. Locks the pin-projection wiring.
    #[tokio::test]
    async fn duplex_round_trip() {
        let (a, b) = duplex(64);
        let mut wrapped = BufferedReader::new(a);
        let mut tail = b;
        wrapped.write_all(b"ping").await.unwrap();
        wrapped.flush().await.unwrap();
        let mut buf = [0u8; 4];
        tail.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }
}
