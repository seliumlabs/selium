//! Mio-based event-driven network poller.
//!
//! Replaces the old tokio::spawn + sleep-based polling with a single
//! mio poller thread. The runtime registers sockets and callbacks;
//! the poller drives the event loop.

use std::{
    collections::HashMap,
    io::{self, Read},
    net::{TcpListener as StdTcpListener, TcpStream as StdTcpStream, UdpSocket as StdUdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use mio::{
    Events, Interest, Token,
    net::{TcpListener, TcpStream, UdpSocket},
};
use parking_lot::Mutex;
use selium_shm::layout::RingWriter;

/// Callback invoked when a TCP listener accepts a new connection.
/// Receives a freshly accepted std TcpStream.
pub type AcceptFn = Box<dyn Fn(StdTcpStream) + Send + 'static>;
/// Callback invoked with the byte count of each socket read, so the runtime
/// can attribute bandwidth consumption to the owning process's metering
/// counter (see `Runtime::record_bandwidth_usage`).
pub type BandwidthFn = Arc<dyn Fn(u64) + Send + Sync + 'static>;
/// Callback invoked when the host advances a ring generation.
pub type GenerationAdvanceFn = Box<dyn Fn(u64, u64) + Send + Sync + 'static>;

/// Poller entry for a registered socket.
enum PollerEntry {
    TcpStream {
        stream: TcpStream,
        inbound_writer: RingWriter,
        region_id: u64,
        running: Arc<AtomicBool>,
        on_bandwidth: Option<BandwidthFn>,
    },
    TcpListener {
        /// Keeps the OS registration alive; dropping it deregisters.
        _mio_listener: TcpListener,
        listener: StdTcpListener,
        accept_fn: AcceptFn,
        running: Arc<AtomicBool>,
    },
    UdpSocket {
        socket: UdpSocket,
        recv_writer: RingWriter,
        region_id: u64,
        running: Arc<AtomicBool>,
    },
}

/// Thread-safe mio poller for event-driven network I/O.
#[derive(Clone)]
pub struct Poller {
    inner: Arc<PollerInner>,
}

/// Shared inner state for the poller.
struct PollerInner {
    poll: Mutex<mio::Poll>,
    entries: Mutex<HashMap<Token, PollerEntry>>,
    next_token: Mutex<usize>,
    generation_advance: Mutex<Option<Arc<GenerationAdvanceFn>>>,
    running: AtomicBool,
}

impl Poller {
    /// Creates a new poller. Call `start_background` to begin the event loop.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            inner: Arc::new(PollerInner {
                poll: Mutex::new(mio::Poll::new()?),
                entries: Mutex::new(HashMap::new()),
                next_token: Mutex::new(0),
                generation_advance: Mutex::new(None),
                running: AtomicBool::new(true),
            }),
        })
    }

    /// Sets the callback invoked after the host advances a ring generation.
    pub fn set_generation_advance<F>(&self, f: F)
    where
        F: Fn(u64, u64) + Send + Sync + 'static,
    {
        *self.inner.generation_advance.lock() = Some(Arc::new(Box::new(f)));
    }

    /// Clears the generation callback, releasing whatever it captured.
    ///
    /// Runtime shutdown calls this after stopping the poller: the callback may
    /// hold a runtime handle, and keeping it in the kernel would form a
    /// runtime → kernel → poller → runtime cycle.
    pub fn clear_generation_advance(&self) {
        *self.inner.generation_advance.lock() = None;
    }

    fn alloc_token(&self) -> Token {
        let mut next = self.inner.next_token.lock();
        let token = Token(*next);
        *next += 1;
        token
    }

    /// Registers a TCP stream for inbound read polling.
    pub fn register_tcp_stream(
        &self,
        stream: StdTcpStream,
        inbound_writer: RingWriter,
        region_id: u64,
        running: Arc<AtomicBool>,
        on_bandwidth: Option<BandwidthFn>,
    ) -> io::Result<()> {
        let mut mio_stream = TcpStream::from_std(stream);
        let token = self.alloc_token();
        self.inner
            .poll
            .lock()
            .registry()
            .register(&mut mio_stream, token, Interest::READABLE)?;
        self.inner.entries.lock().insert(
            token,
            PollerEntry::TcpStream {
                stream: mio_stream,
                inbound_writer,
                region_id,
                running,
                on_bandwidth,
            },
        );
        Ok(())
    }

    /// Registers a TCP listener. The mio wrapper MUST be kept alive for as
    /// long as the registration should exist: dropping it closes the fd,
    /// which removes the OS registration (kqueue deletes the knote when the
    /// registered fd is closed).
    pub fn register_tcp_listener(
        &self,
        listener: StdTcpListener,
        accept_fn: AcceptFn,
        running: Arc<AtomicBool>,
    ) -> io::Result<()> {
        // Register the listener's fd with mio for readability.
        let mut mio_listener = TcpListener::from_std(listener.try_clone()?);
        let token = self.alloc_token();
        self.inner
            .poll
            .lock()
            .registry()
            .register(&mut mio_listener, token, Interest::READABLE)?;
        // Store the mio wrapper (keeps the registration alive) plus a std
        // clone for actual accept calls.
        self.inner.entries.lock().insert(
            token,
            PollerEntry::TcpListener {
                _mio_listener: mio_listener,
                listener,
                accept_fn,
                running,
            },
        );
        Ok(())
    }

    /// Registers a UDP socket for inbound recv polling.
    pub fn register_udp_socket(
        &self,
        socket: StdUdpSocket,
        recv_writer: RingWriter,
        region_id: u64,
        running: Arc<AtomicBool>,
    ) -> io::Result<()> {
        let mut mio_socket = UdpSocket::from_std(socket);
        let token = self.alloc_token();
        self.inner
            .poll
            .lock()
            .registry()
            .register(&mut mio_socket, token, Interest::READABLE)?;
        self.inner.entries.lock().insert(
            token,
            PollerEntry::UdpSocket {
                socket: mio_socket,
                recv_writer,
                region_id,
                running,
            },
        );
        Ok(())
    }

    /// Starts the event loop on the current thread (blocks until shutdown).
    pub fn run(&self) {
        let mut events = Events::with_capacity(64);
        let poll_timeout = Some(Duration::from_millis(100));

        while self.inner.running.load(Ordering::Relaxed) {
            if let Err(e) = self.inner.poll.lock().poll(&mut events, poll_timeout) {
                eprintln!("mio poll error: {e}");
                break;
            }
            for event in events.iter() {
                if event.is_readable() {
                    self.handle_readable(event.token());
                }
            }

            // A UDP datagram left in the kernel buffer after the ring filled
            // does not produce a fresh edge-triggered event. Retry registered
            // UDP sockets on the poller's bounded 100 ms cadence so preserved
            // datagrams resume when the guest advances its reader.
            let udp_tokens: Vec<Token> = self
                .inner
                .entries
                .lock()
                .iter()
                .filter_map(|(token, entry)| {
                    matches!(entry, PollerEntry::UdpSocket { .. }).then_some(*token)
                })
                .collect();
            for token in udp_tokens {
                self.handle_readable(token);
            }
        }
    }

    /// Starts the event loop on a background thread. Returns a join handle.
    pub fn start_background(self) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            self.run();
        })
    }

    /// Shuts down the poller event loop.
    pub fn shutdown(&self) {
        self.inner.running.store(false, Ordering::Relaxed);
    }

    /// Services one readiness event.
    ///
    /// mio readiness is edge-triggered (kqueue `EV_CLEAR` / epoll `EPOLLET`):
    /// a single event stands for *everything* queued on the socket at the
    /// time it is retrieved, and no further event is generated for those
    /// bytes or datagrams. Every branch below therefore drains its socket
    /// until `WouldBlock`. Reading once per event strands whatever else was
    /// queued — typically several datagrams that arrived while the guest was
    /// being executed inline on this thread — until the *next* arrival,
    /// leaving the guest permanently one or more packets behind its peer (a
    /// QUIC peer then waits forever for a reply to a packet the poller has
    /// but never delivers).
    fn handle_readable(&self, token: Token) {
        let advance = self.inner.generation_advance.lock().clone();

        // Take the entry out of the map while processing. Socket IO and the
        // accept callback must run without holding `entries`: callbacks can
        // register new sockets (and may even re-enter this poller from a
        // guest reactor executed inline), which would self-deadlock on the
        // non-reentrant mutex.
        let entry = self.inner.entries.lock().remove(&token);
        match entry {
            Some(PollerEntry::TcpStream {
                mut stream,
                inbound_writer,
                region_id,
                running,
                on_bandwidth,
            }) => {
                if !running.load(Ordering::Relaxed) {
                    // Stopped externally (close_tcp_stream): clean up the
                    // registration instead of lingering forever.
                    self.deregister(&mut stream, token);
                    return;
                }
                let mut buf = vec![0u8; 8192];
                let mut finished = false;
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) => {
                            drop(inbound_writer.decrement_writer_count());
                            // Bump the generation so readers parked on this ring
                            // (via WaitRegister) are woken and observe EOF;
                            // writer-count changes alone do not wake anyone.
                            let new_gen = inbound_writer
                                .backend()
                                .fetch_add_u64(
                                    selium_shm::layout::GENERATION_COUNTER_OFFSET,
                                    1,
                                    Ordering::Release,
                                )
                                .map(|g| g.wrapping_add(1))
                                .ok();
                            // A clean EOF half-closes only the remote's write
                            // direction. Deregister this inbound socket, but
                            // leave the shared running flag set so the outbound
                            // drainer can deliver replies until the guest closes
                            // its writer.
                            finished = true;
                            if let (Some(new_gen), Some(cb)) = (new_gen, advance.as_ref()) {
                                cb(region_id, new_gen);
                            }
                            break;
                        }
                        Ok(n) => {
                            if let Some(record) = on_bandwidth.as_ref() {
                                record(n as u64);
                            }
                            let payload = buf.get(..n).unwrap_or(&[]);
                            if inbound_writer.write_frame(payload, 0, 0).is_err() {
                                eprintln!(
                                    "inbound ring full for region {region_id}; dropped {n} bytes"
                                );
                            } else if let Some(cb) = advance.as_ref()
                                && let Ok(new_gen) = inbound_writer.generation()
                            {
                                // Wake (and inline-execute) the guest per chunk so
                                // it consumes the ring before the next write.
                                cb(region_id, new_gen);
                            }
                        }
                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(e) => {
                            // A reset/abort is a connection end, not a ring
                            // failure: readers must still observe EOF rather
                            // than park forever on a peer that vanished.
                            let connection_ended = matches!(
                                e.kind(),
                                io::ErrorKind::ConnectionReset
                                    | io::ErrorKind::ConnectionAborted
                                    | io::ErrorKind::BrokenPipe
                            );
                            if connection_ended {
                                drop(inbound_writer.decrement_writer_count());
                                let new_gen = inbound_writer
                                    .backend()
                                    .fetch_add_u64(
                                        selium_shm::layout::GENERATION_COUNTER_OFFSET,
                                        1,
                                        Ordering::Release,
                                    )
                                    .map(|g| g.wrapping_add(1))
                                    .ok();
                                if let (Some(new_gen), Some(cb)) = (new_gen, advance.as_ref()) {
                                    cb(region_id, new_gen);
                                }
                            } else {
                                eprintln!("inbound TCP read error: {e}");
                            }
                            running.store(false, Ordering::Relaxed);
                            finished = true;
                            break;
                        }
                    }
                }
                if finished {
                    self.deregister(&mut stream, token);
                } else {
                    self.reinsert(
                        token,
                        PollerEntry::TcpStream {
                            stream,
                            inbound_writer,
                            region_id,
                            running,
                            on_bandwidth,
                        },
                    );
                }
            }
            Some(mut entry @ PollerEntry::TcpListener { .. }) => {
                // Split out the fields we mutate, keeping `_mio_listener`
                // alive inside `entry` so the registration stays active.
                #[expect(
                    clippy::unreachable,
                    reason = "variant matched above; irrefutable otherwise"
                )]
                let PollerEntry::TcpListener {
                    listener,
                    accept_fn,
                    running,
                    ..
                } = &mut entry
                else {
                    unreachable!("matched above");
                };
                if !running.load(Ordering::Relaxed) {
                    // Stopped externally: drop the entry, freeing the fd.
                    return;
                }
                // Accept via the std listener, not mio — we get a std
                // TcpStream that the callback can use directly. Drain the
                // accept queue: one edge may cover several pending connections.
                loop {
                    match listener.accept() {
                        Ok((stream, _addr)) => accept_fn(stream),
                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(e) => {
                            eprintln!("accept error: {e}");
                            running.store(false, Ordering::Relaxed);
                            return;
                        }
                    }
                }
                self.reinsert(token, entry);
            }
            Some(PollerEntry::UdpSocket {
                socket,
                recv_writer,
                region_id,
                running,
            }) => {
                if !running.load(Ordering::Relaxed) {
                    // Stopped externally: clean up the registration.
                    let mut socket = socket;
                    self.deregister_udp(&mut socket, token);
                    return;
                }
                let mut buf = vec![0u8; 65536];
                let mut finished = false;
                loop {
                    // Peek first: a full ring must leave the datagram in the
                    // kernel receive buffer. Reading it and only then finding
                    // the ring full turns backpressure into packet loss.
                    let (n, addr) = match socket.peek_from(&mut buf) {
                        Ok(next) => next,
                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            eprintln!("UDP peek error: {e}");
                            running.store(false, Ordering::Relaxed);
                            finished = true;
                            break;
                        }
                    };
                    let address_header = match addr {
                        std::net::SocketAddr::V4(_) => 8,
                        std::net::SocketAddr::V6(_) => 20,
                    };
                    if !recv_writer
                        .has_capacity_for_frame((n + address_header) as u64)
                        .unwrap_or(false)
                    {
                        break;
                    }
                    match socket.recv_from(&mut buf) {
                        Ok((n, addr)) => {
                            let payload = buf.get(..n).unwrap_or(&[]);
                            let frame = crate::network::encode_udp_frame(addr, payload);
                            recv_writer
                                .write_frame(&frame, 0, 0)
                                .expect("capacity was checked before consuming the datagram");
                            if let Some(cb) = advance.as_ref()
                                && let Ok(new_gen) = recv_writer.generation()
                            {
                                // Wake (and inline-execute) the guest per datagram
                                // so it consumes the ring before the next write.
                                cb(region_id, new_gen);
                            }
                        }
                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(e) => {
                            eprintln!("UDP recv error: {e}");
                            running.store(false, Ordering::Relaxed);
                            finished = true;
                            break;
                        }
                    }
                }
                if finished {
                    let mut socket = socket;
                    self.deregister_udp(&mut socket, token);
                } else {
                    self.reinsert(
                        token,
                        PollerEntry::UdpSocket {
                            socket,
                            recv_writer,
                            region_id,
                            running,
                        },
                    );
                }
            }
            None => {}
        }
    }

    /// Puts a still-live entry back into the map after processing.
    fn reinsert(&self, token: Token, entry: PollerEntry) {
        self.inner.entries.lock().insert(token, entry);
    }

    /// Removes a finished TCP stream's fd from the mio registry so the
    /// token/socket are not leaked.
    fn deregister(&self, stream: &mut mio::net::TcpStream, token: Token) {
        let result = self.inner.poll.lock().registry().deregister(stream);
        if let Err(e) = result {
            eprintln!("poller deregister failed for token {:?}: {e}", token.0);
        }
    }

    fn deregister_udp(&self, socket: &mut mio::net::UdpSocket, token: Token) {
        let result = self.inner.poll.lock().registry().deregister(socket);
        if let Err(e) = result {
            eprintln!("poller deregister failed for token {:?}: {e}", token.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream as StdClient;
    use std::time::Instant;

    // scenario: selium-kernel/event-driven-network-poller/accept-is-event-driven
    #[test]
    fn listener_registration_delivers_accept_events() {
        let poller = Poller::new().expect("poller");
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        listener.set_nonblocking(true).unwrap();

        let accepted = Arc::new(AtomicBool::new(false));
        let accepted2 = accepted.clone();
        poller
            .register_tcp_listener(
                listener,
                Box::new(move |_s| {
                    accepted2.store(true, Ordering::Relaxed);
                }),
                Arc::new(AtomicBool::new(true)),
            )
            .expect("register");
        poller.start_background();

        let _client = StdClient::connect(addr).expect("connect");
        let start = Instant::now();
        while !accepted.load(Ordering::Relaxed) && start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            accepted.load(Ordering::Relaxed),
            "accept callback never fired"
        );
    }

    #[test]
    fn udp_full_ring_preserves_datagrams_and_resumes_after_drain() {
        use selium_shm::layout::{self, RingReader};

        let backend: Arc<dyn selium_memory::MappingBackend> = Arc::new(
            selium_memory::PointerBackend::allocate(4096 + 256).expect("allocate UDP ring"),
        );
        layout::init_ring(backend.as_ref()).expect("initialise ring");
        layout::store_capacity(backend.as_ref(), 256).expect("store capacity");
        let writer = RingWriter::open(backend.clone(), 256).expect("writer");
        let mut reader = RingReader::open(backend, 256, true).expect("blocking reader");

        let socket = StdUdpSocket::bind("127.0.0.1:0").expect("bind receiver");
        socket.set_nonblocking(true).expect("nonblocking receiver");
        let address = socket.local_addr().expect("receiver address");
        let poller = Poller::new().expect("poller");
        poller
            .register_udp_socket(socket, writer, 42, Arc::new(AtomicBool::new(true)))
            .expect("register UDP");

        let sender = StdUdpSocket::bind("127.0.0.1:0").expect("bind sender");
        for _ in 0..3 {
            sender.send_to(&[7u8; 128], address).expect("send burst");
        }
        poller.clone().start_background();

        // The ring holds one 128-byte frame. The poller must not consume and
        // drop the other two: they stay in the kernel receive buffer until the
        // reader advances and capacity returns.
        let start = Instant::now();
        let mut received = 0;
        while received < 3 {
            if reader.read_frame().expect("read frame").is_some() {
                received += 1;
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "only {received}/3 datagrams survived ring backpressure"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        poller.shutdown();
    }
}
