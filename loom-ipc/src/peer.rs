use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};

use mio::net::UnixStream;

use crate::message::Message;

/// Buffer size for reading message data.
const READ_BUF_SIZE: usize = 65536;

/// A peer connection over a Unix domain socket.
///
/// Wraps a `mio::net::UnixStream` with framed message serialization
/// (4-byte big-endian length prefix + bincode payload).
pub struct Peer {
    stream: UnixStream,
    /// Partial message data being accumulated.
    recv_buf: Vec<u8>,
    /// Expected length of current incoming message (payload only).
    recv_len: Option<usize>,
    /// Whether we've read the length prefix for the current message.
    recv_have_len: bool,
    /// Outgoing message queue.
    send_queue: VecDeque<Vec<u8>>,
    /// Peer flags.
    pub flags: u8,
    /// Peer uid (set after peer is established).
    pub uid: Option<u32>,
    /// Peer gid.
    pub gid: Option<u32>,
}

impl Peer {
    /// Create a new peer from an established UnixStream.
    pub fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            recv_buf: Vec::with_capacity(READ_BUF_SIZE),
            recv_len: None,
            recv_have_len: false,
            send_queue: VecDeque::new(),
            flags: 0,
            uid: None,
            gid: None,
        }
    }

    /// Register this peer's stream with a mio `Registry`.
    pub fn register(
        &mut self,
        registry: &mio::Registry,
        token: mio::Token,
        interests: mio::Interest,
    ) -> io::Result<()> {
        registry.register(&mut self.stream, token, interests)
    }

    /// Reregister with updated interests.
    pub fn reregister(
        &mut self,
        registry: &mio::Registry,
        token: mio::Token,
        interests: mio::Interest,
    ) -> io::Result<()> {
        registry.reregister(&mut self.stream, token, interests)
    }

    /// Deregister from the event loop.
    pub fn deregister(&mut self, registry: &mio::Registry) -> io::Result<()> {
        registry.deregister(&mut self.stream)
    }

    /// Send a message to the peer.
    ///
    /// Serializes with bincode, prepends 4-byte length.
    /// Returns `Ok(true)` if fully written, `Ok(false)` if queued.
    pub fn send(&mut self, msg: &Message) -> bincode::Result<bool> {
        let payload = bincode::serialize(msg)?;
        let len = payload.len() as u32;
        let mut frame = Vec::with_capacity(4 + payload.len());
        frame.extend_from_slice(&len.to_be_bytes());
        frame.extend_from_slice(&payload);

        if self.send_queue.is_empty() {
            match self.stream.write(&frame) {
                Ok(n) if n == frame.len() => return Ok(true),
                Ok(n) => {
                    self.send_queue.push_back(frame[n..].to_vec());
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.send_queue.push_back(frame);
                }
                Err(e) => return Err(Box::new(bincode::ErrorKind::Io(e))),
            }
        } else {
            self.send_queue.push_back(frame);
        }
        Ok(false)
    }

    /// Try to read a complete message from the peer.
    ///
    /// Returns `Ok(Some(msg))` if a complete message is available,
    /// `Ok(None)` if more data is needed, `Err` on protocol error.
    pub fn recv(&mut self) -> io::Result<Option<Message>> {
        loop {
            // Read length prefix (4 bytes)
            if !self.recv_have_len {
                let needed = 4 - self.recv_buf.len();
                if needed > 0 {
                    let mut buf = vec![0u8; needed];
                    match self.stream.read(&mut buf) {
                        Ok(0) => {
                            return if self.recv_buf.is_empty() {
                                Ok(None)
                            } else {
                                Err(io::Error::new(
                                    io::ErrorKind::UnexpectedEof,
                                    "peer disconnected mid-message",
                                ))
                            };
                        }
                        Ok(n) => {
                            self.recv_buf.extend_from_slice(&buf[..n]);
                            if self.recv_buf.len() < 4 {
                                return Ok(None);
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            return Ok(None);
                        }
                        Err(e) => return Err(e),
                    }
                }
                // Parse length
                let len_bytes: [u8; 4] = self.recv_buf[..4].try_into().unwrap();
                let len = u32::from_be_bytes(len_bytes) as usize;
                if len > 16 * 1024 * 1024 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("message too large: {} bytes", len),
                    ));
                }
                self.recv_len = Some(len);
                self.recv_have_len = true;
                self.recv_buf.clear();
            }

            // Read payload
            let needed = self.recv_len.unwrap();
            if self.recv_buf.len() < needed {
                let mut buf = vec![0u8; (needed - self.recv_buf.len()).min(READ_BUF_SIZE)];
                match self.stream.read(&mut buf) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "peer disconnected mid-message",
                        ));
                    }
                    Ok(n) => {
                        self.recv_buf.extend_from_slice(&buf[..n]);
                        if self.recv_buf.len() < needed {
                            return Ok(None);
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        return Ok(None);
                    }
                    Err(e) => return Err(e),
                }
            }

            // Complete message received
            let len = self.recv_len.take().unwrap();
            self.recv_have_len = false;
            let payload = self.recv_buf.drain(..len).collect::<Vec<_>>();

            match bincode::deserialize(&payload) {
                Ok(msg) => return Ok(Some(msg)),
                Err(e) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("deserialize failed: {}", e),
                    ));
                }
            }
        }
    }

    /// Flush the send queue, writing queued data to the socket.
    ///
    /// Returns `Ok(true)` if all data was flushed.
    pub fn flush(&mut self) -> io::Result<bool> {
        while let Some(data) = self.send_queue.front_mut() {
            match self.stream.write(data) {
                Ok(n) if n == data.len() => {
                    self.send_queue.pop_front();
                }
                Ok(n) => {
                    data.drain(..n);
                    return Ok(false);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    return Ok(false);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }

    /// Check if there is pending data to write.
    pub fn has_pending_writes(&self) -> bool {
        !self.send_queue.is_empty()
    }

    /// Get the raw fd of the underlying socket.
    pub fn as_raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }
}

impl AsRawFd for Peer {
    fn as_raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream as StdUnixStream;

    fn create_pair() -> (Peer, Peer) {
        let (a, b) = StdUnixStream::pair().unwrap();
        (Peer::new(UnixStream::from_std(a)), Peer::new(UnixStream::from_std(b)))
    }

    #[test]
    fn test_send_recv() {
        let (mut p1, mut p2) = create_pair();

        p1.send(&Message::IdentifyFlags(42)).unwrap();
        p1.flush().unwrap();

        let msg = p2.recv().unwrap().unwrap();
        assert!(matches!(msg, Message::IdentifyFlags(42)));
    }

    #[test]
    fn test_command_roundtrip() {
        let (mut p1, mut p2) = create_pair();

        let cmd = Message::Command {
            argc: 2,
            argv: vec!["new-session".into(), "-s".into()],
        };
        p1.send(&cmd).unwrap();
        p1.flush().unwrap();

        let msg = p2.recv().unwrap().unwrap();
        match msg {
            Message::Command { argc, argv } => {
                assert_eq!(argc, 2);
                assert_eq!(argv[0], "new-session");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_multiple_messages() {
        let (mut p1, mut p2) = create_pair();

        p1.send(&Message::Ready).unwrap();
        p1.send(&Message::Resize { sx: 80, sy: 24 }).unwrap();
        p1.flush().unwrap();

        let m1 = p2.recv().unwrap().unwrap();
        assert!(matches!(m1, Message::Ready));

        let m2 = p2.recv().unwrap().unwrap();
        assert!(matches!(m2, Message::Resize { sx: 80, sy: 24 }));
    }

    #[test]
    fn test_send_queue() {
        let (mut p1, mut p2) = create_pair();

        // Use large messages to overflow the kernel buffer.
        let large = Message::IdentifyTerminfo(
            (0..200).map(|i| (format!("cap{}", i), format!("val{}", i))).collect(),
        );
        p1.send(&large).unwrap();

        // Flush and verify at least one message can be received.
        p1.flush().unwrap();
        assert!(!p1.has_pending_writes());
        assert!(p2.recv().unwrap().is_some());
    }

    #[test]
    fn test_large_message() {
        let (mut p1, mut p2) = create_pair();

        let large = Message::IdentifyTerminfo(
            (0..1000).map(|i| (format!("cap{}", i), format!("val{}", i))).collect(),
        );
        p1.send(&large).unwrap();
        p1.flush().unwrap();

        let msg = p2.recv().unwrap().unwrap();
        match msg {
            Message::IdentifyTerminfo(pairs) => {
                assert_eq!(pairs.len(), 1000);
                assert_eq!(pairs[0].0, "cap0");
            }
            _ => panic!("wrong variant"),
        }
    }
}
