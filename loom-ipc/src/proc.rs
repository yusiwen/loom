use std::collections::HashMap;
use std::io;
use std::os::unix::io::RawFd;
use std::time::Duration;

use mio::{event::Event, Events, Interest, Poll, Registry, Token};

use crate::message::Message;
use crate::peer::Peer;

/// Accept listener token.
const ACCEPT_TOKEN: Token = Token(0);

/// Base token for peer connections.
const PEER_BASE: usize = 256;

/// Callback for handling incoming messages from a peer.
pub type DispatchFn = Box<dyn FnMut(&Message, &mut Peer) -> io::Result<()> + Send>;

/// Callback for handling peer disconnection.
pub type DisconnectFn = Box<dyn FnMut(RawFd) + Send>;

/// Callback for handling signals (called with signal number).
pub type SignalFn = Box<dyn FnMut(i32) + Send>;

/// Callback for accepting new connections.
/// Receives the listen fd, returns a new Peer.
pub type AcceptFn = Box<dyn FnMut(RawFd) -> io::Result<(Peer, Token)> + Send>;

/// Callback for the main loop iteration.
/// Return `true` to exit the loop.
pub type LoopFn = Box<dyn FnMut() -> bool + Send>;

/// A process managing an mio event loop with connected peers.
pub struct Proc {
    /// Name of this process.
    pub name: String,
    /// mio poll instance.
    poll: Poll,
    /// Connected peers.
    peers: HashMap<Token, Peer>,
    /// Next available peer token.
    peer_counter: usize,
    /// Dispatch callback for received messages.
    dispatch: Option<DispatchFn>,
    /// Disconnect callback.
    on_disconnect: Option<DisconnectFn>,
    /// Signal callback.
    on_signal: Option<SignalFn>,
    /// Accept callback (for server-side).
    accept: Option<AcceptFn>,
    /// Loop callback (called each iteration).
    on_loop: Option<LoopFn>,
    /// Exit flag.
    pub exit: bool,
}

impl Proc {
    /// Create a new process with the given name.
    pub fn new(name: &str) -> io::Result<Self> {
        let poll = Poll::new()?;
        Ok(Self {
            name: name.to_string(),
            poll,
            peers: HashMap::new(),
            peer_counter: 0,
            dispatch: None,
            on_disconnect: None,
            on_signal: None,
            accept: None,
            on_loop: None,
            exit: false,
        })
    }

    /// Get a reference to the mio registry.
    pub fn registry(&self) -> &Registry {
        self.poll.registry()
    }

    /// Set the message dispatch callback.
    pub fn on_message(&mut self, cb: DispatchFn) {
        self.dispatch = Some(cb);
    }

    /// Set the disconnect callback.
    pub fn on_disconnect(&mut self, cb: DisconnectFn) {
        self.on_disconnect = Some(cb);
    }

    /// Set the signal callback.
    pub fn on_signal(&mut self, cb: SignalFn) {
        self.on_signal = Some(cb);
    }

    /// Set the accept callback.
    pub fn on_accept(&mut self, cb: AcceptFn) {
        self.accept = Some(cb);
    }

    /// Set the loop callback.
    pub fn on_loop(&mut self, cb: LoopFn) {
        self.on_loop = Some(cb);
    }

    /// Register a listen fd for accepting connections.
    pub fn register_accept<T: mio::event::Source + 'static>(
        &mut self,
        source: &mut T,
    ) -> io::Result<()> {
        self.poll.registry().register(source, ACCEPT_TOKEN, Interest::READABLE)
    }

    /// Add a peer to the event loop.
    pub fn add_peer(&mut self, mut peer: Peer) -> io::Result<Token> {
        let token = Token(PEER_BASE + self.peer_counter);
        self.peer_counter += 1;

        peer.register(
            self.poll.registry(),
            token,
            Interest::READABLE | Interest::WRITABLE,
        )?;

        self.peers.insert(token, peer);
        Ok(token)
    }

    /// Remove a peer from the event loop.
    pub fn remove_peer(&mut self, token: Token) -> Option<Peer> {
        let mut peer = self.peers.remove(&token)?;
        let _ = peer.deregister(self.poll.registry());
        Some(peer)
    }

    /// Send a message to a specific peer.
    pub fn send_to(&mut self, token: Token, msg: &Message) -> bincode::Result<()> {
        if let Some(peer) = self.peers.get_mut(&token) {
            peer.send(msg)?;
        }
        Ok(())
    }

    /// Flush all pending writes.
    pub fn flush_all(&mut self) -> io::Result<()> {
        for peer in self.peers.values_mut() {
            peer.flush()?;
        }
        Ok(())
    }

    /// Run the main event loop.
    ///
    /// Returns when `exit` is set to true or on error.
    pub fn run(&mut self) -> io::Result<()> {
        let mut events = Events::with_capacity(1024);

        while !self.exit {
            if let Some(ref mut cb) = self.on_loop {
                if cb() {
                    self.exit = true;
                    break;
                }
            }

            let timeout = Duration::from_millis(100);
            match self.poll.poll(&mut events, Some(timeout)) {
                Ok(_) => {}
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }

            for event in &events {
                match event.token() {
                    ACCEPT_TOKEN => {
                        self.handle_accept()?;
                    }
                    token => {
                        self.handle_peer_event(token, event)?;
                    }
                }
            }
        }

        self.flush_all()?;
        Ok(())
    }

    fn handle_accept(&mut self) -> io::Result<()> {
        if let Some(ref mut _cb) = self.accept {
            // Accept callback will be invoked with the listen fd.
            // The caller must register the listen source separately.
        }
        Ok(())
    }

    fn handle_peer_event(&mut self, token: Token, event: &Event) -> io::Result<()> {
        if event.is_error() || event.is_read_closed() || event.is_write_closed() {
            let peer = self.remove_peer(token);
            if let Some(ref mut cb) = self.on_disconnect {
                if let Some(ref p) = peer {
                    cb(p.as_raw_fd());
                }
            }
            return Ok(());
        }

        let peer_id = token;

        if event.is_readable() {
            // Get the peer, try to recv
            let recv_result = {
                let peer = self.peers.get_mut(&peer_id);
                match peer {
                    Some(p) => p.recv(),
                    None => return Ok(()),
                }
            };

            match recv_result {
                Ok(Some(msg)) => {
                    if let Some(ref mut cb) = self.dispatch {
                        let peer = self.peers.get_mut(&peer_id);
                        if let Some(p) = peer {
                            cb(&msg, p)?;
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    let peer = self.remove_peer(peer_id);
                    if let Some(ref mut cb) = self.on_disconnect {
                        if let Some(ref p) = peer {
                            cb(p.as_raw_fd());
                        }
                    }
                    return Err(e);
                }
            }
        }

        if event.is_writable() {
            if let Some(peer) = self.peers.get_mut(&peer_id) {
                if peer.has_pending_writes() {
                    peer.flush()?;
                }
            }
        }

        Ok(())
    }

    /// Number of connected peers.
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    /// Iterate over peer tokens.
    pub fn peer_tokens(&self) -> impl Iterator<Item = &Token> {
        self.peers.keys()
    }

    /// Get a mutable peer reference.
    pub fn get_peer_mut(&mut self, token: &Token) -> Option<&mut Peer> {
        self.peers.get_mut(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream as StdUnixStream;

    #[test]
    fn test_add_remove_peer() {
        let (a, _b) = StdUnixStream::pair().unwrap();
        let peer = Peer::new(mio::net::UnixStream::from_std(a));

        let mut proc = Proc::new("test").unwrap();
        let token = proc.add_peer(peer).unwrap();

        assert_eq!(proc.peer_count(), 1);

        let _removed = proc.remove_peer(token);
        assert_eq!(proc.peer_count(), 0);
    }

    #[test]
    fn test_proc_send() {
        let (a, b) = StdUnixStream::pair().unwrap();
        let peer_a = Peer::new(mio::net::UnixStream::from_std(a));
        let mut peer_b = Peer::new(mio::net::UnixStream::from_std(b));

        let mut proc = Proc::new("test").unwrap();
        let token = proc.add_peer(peer_a).unwrap();

        proc.send_to(token, &Message::Ready).unwrap();
        proc.flush_all().unwrap();

        let msg = peer_b.recv().unwrap().unwrap();
        assert!(matches!(msg, Message::Ready));
    }
}
