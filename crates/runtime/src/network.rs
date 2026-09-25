//! Async network I/O for the Selium runtime.
//!
//! Uses an mio-based event poller (in selium-kernel) for inbound socket
//! readiness, and blocking std threads for outbound ring-to-socket draining
//! with condvar-based generation waits.

use std::{
    io::Write,
    net::{TcpStream, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use selium_abi::{HostQueueDescriptor, SharedRegionDescriptor};
use selium_kernel::Kernel;
use selium_memory::{MappingBackend, MultiMemoryHeader};
use selium_shm::layout::{self, GENERATION_COUNTER_OFFSET, RingReader, RingWriter};

use crate::{error::Result, runtime::Runtime};

/// Default ring buffer data capacity (64 KiB, power of two).
const DEFAULT_RING_CAPACITY: u64 = 64 * 1024;
/// Maximum time to wait on a generation advance for outbound drain (ms).
/// The backstop ensures correctness even if a runtime kick is missed.
const OUTBOUND_WAIT_TIMEOUT_MS: u64 = 1000;

/// Bind a TCP listener and register it with the mio poller.
pub fn tcp_bind(
    runtime: &Runtime,
    process_id: u64,
    address: String,
) -> Result<HostQueueDescriptor> {
    let kernel = &runtime.kernel;
    let std_listener = std::net::TcpListener::bind(&address)
        .map_err(|e| crate::Error::Host(format!("tcp bind failed: {e}")))?;
    std_listener
        .set_nonblocking(true)
        .map_err(|e| crate::Error::Host(format!("set_nonblocking failed: {e}")))?;

    let memory = kernel.memory();
    let queues = kernel.queues();
    let descriptor = queues.create_host_queue(&memory);

    let local_id = descriptor.local_id;
    // The guest parked receiving on this queue will need waking when the
    // poller enqueues an accepted connection.
    runtime.register_queue_waiter(local_id, process_id);
    let running = Arc::new(AtomicBool::new(true));

    kernel.network().insert_tcp_listener(
        local_id,
        selium_kernel::TcpListenerState {
            shared_id: descriptor.shared_id,
            running: running.clone(),
            _listener: std_listener
                .try_clone()
                .map_err(|e| crate::Error::Host(format!("listener clone failed: {e}")))?,
        },
    );

    // Register the listener with the mio poller.
    if let Some(poller) = kernel.poller() {
        let k = kernel.clone();
        let rt = runtime.clone();
        let queue_local_id = local_id;
        // Bandwidth attribution: bytes on accepted connections belong to the
        // listener's owner (the accepting process's tenant).
        let owner_pid = process_id;
        poller
            .register_tcp_listener(
                std_listener,
                Box::new(move |stream: std::net::TcpStream| {
                    // A queued connection reserves one pipe slot against the
                    // listener owner's (serving) tenant. Over-ceiling arrivals
                    // are dropped at the chokepoint — the external peer sees a
                    // closed connection — and the denial is logged to aid
                    // debugging.
                    if let Ok(queue_shared_id) = k.queues().host_queue_shared_id(queue_local_id)
                        && let Err(error) = rt.charge_queue_item(queue_shared_id)
                    {
                        tracing::debug!(
                            queue = queue_shared_id,
                            error = error.message,
                            "accepted connection dropped: listener tenant's pipe quota exhausted"
                        );
                        return;
                    }
                    drop(stream.set_nonblocking(true));
                    let (region, inbound_writer, outbound_reader, ring_offset, parent_local_id) =
                        match create_stream_region(&k) {
                            Ok(r) => r,
                            Err(e) => {
                                eprintln!("failed to create stream region: {e}");
                                return;
                            }
                        };

                    let shared_id = region.shared_id;
                    let stream_running = Arc::new(AtomicBool::new(true));

                    k.network().insert_tcp_stream(
                        shared_id,
                        selium_kernel::TcpStreamState {
                            running: stream_running.clone(),
                        },
                    );

                    // Clone the stream for outbound writes.
                    let outbound_stream = match stream.try_clone() {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("failed to clone accepted stream: {e}");
                            let memory = k.memory();
                            drop(memory.detach_shared_region(parent_local_id));
                            k.network().remove_tcp_stream(shared_id);
                            return;
                        }
                    };

                    // Register the accepted stream for inbound reads with the
                    // poller. Failure is fatal for this connection: without an
                    // inbound pump the guest would wait forever for data that is
                    // never delivered, so tear the connection down loudly rather
                    // than letting it hang.
                    let bandwidth_rt = rt.clone();
                    let bandwidth_cb: Option<selium_kernel::BandwidthFn> =
                        Some(std::sync::Arc::new(move |bytes: u64| {
                            bandwidth_rt.record_bandwidth_usage(owner_pid, bytes);
                        }));
                    let registration = match k.poller() {
                        Some(p) => p.register_tcp_stream(
                            stream,
                            inbound_writer,
                            shared_id,
                            stream_running.clone(),
                            bandwidth_cb,
                        ),
                        None => Err(std::io::Error::other("network poller not initialised")),
                    };
                    if let Err(e) = registration {
                        eprintln!(
                            "accepted connection dropped: inbound poller registration failed: {e}"
                        );
                        drop(outbound_stream.shutdown(std::net::Shutdown::Both));
                        stream_running.store(false, Ordering::Relaxed);
                        let memory = k.memory();
                        drop(memory.detach_shared_region(parent_local_id));
                        k.network().remove_tcp_stream(shared_id);
                        return;
                    }

                    // Register the wait target for runtime kicks: the
                    // generation-word offset within the shared region.
                    let gen_offset = ring_offset + GENERATION_COUNTER_OFFSET;
                    rt.network_wait_keys.lock().push((shared_id, gen_offset));

                    // Spawn outbound drain on a dedicated thread.
                    let memory = k.memory();
                    let rt2 = rt.clone();
                    let drain_rt = rt.clone();
                    let drain_pid = owner_pid;
                    thread::spawn(move || {
                        if let Err(_e) = proxy_outbound_tcp(
                            outbound_stream,
                            outbound_reader,
                            stream_running,
                            Some(Box::new(move |bytes: u64| {
                                drain_rt.record_bandwidth_usage(drain_pid, bytes);
                            })),
                        ) {}
                        rt2.network_wait_keys
                            .lock()
                            .retain(|(sid, _)| *sid != shared_id);
                        drop(memory.detach_shared_region(parent_local_id));
                    });

                    if let Err(e) =
                        k.queues()
                            .host_queue_send(queue_local_id, 0, shared_id, Vec::new())
                    {
                        eprintln!("failed to enqueue connection: {e}");
                    } else {
                        // The connection is queued: wake the guest parked on
                        // HostQueueRecv so it re-polls and completes its accept.
                        rt.wake_queue_waiter(queue_local_id);
                    }
                }),
                running,
            )
            .map_err(|e| crate::Error::Host(format!("poller register listener: {e}")))?;
    }

    Ok(descriptor)
}

/// Connect to a TCP endpoint and register the stream with the mio poller.
///
/// `process_id` is the connecting process: its metering counter is credited
/// with the stream's bandwidth in both directions.
pub fn tcp_connect(
    runtime: &Runtime,
    process_id: u64,
    address: String,
) -> Result<SharedRegionDescriptor> {
    let kernel = &runtime.kernel;
    let std_stream = TcpStream::connect(&address)
        .map_err(|e| crate::Error::Host(format!("tcp connect failed: {e}")))?;
    std_stream
        .set_nonblocking(true)
        .map_err(|e| crate::Error::Host(format!("tcp set_nonblocking failed: {e}")))?;

    let outbound_stream = std_stream
        .try_clone()
        .map_err(|e| crate::Error::Host(format!("tcp stream clone failed: {e}")))?;

    let (descriptor, inbound_writer, outbound_reader, ring_offset, parent_local_id) =
        create_stream_region(kernel)?;

    let shared_id = descriptor.shared_id;
    let running = Arc::new(AtomicBool::new(true));

    kernel.network().insert_tcp_stream(
        shared_id,
        selium_kernel::TcpStreamState {
            running: running.clone(),
        },
    );

    // Register the stream for inbound reads with the mio poller. Inbound
    // bytes are attributed to the connecting process's bandwidth counter.
    if let Some(poller) = kernel.poller() {
        let bandwidth_rt = runtime.clone();
        let bandwidth_pid = process_id;
        let bandwidth_cb: Option<selium_kernel::BandwidthFn> =
            Some(std::sync::Arc::new(move |bytes: u64| {
                bandwidth_rt.record_bandwidth_usage(bandwidth_pid, bytes)
            }));
        poller
            .register_tcp_stream(
                std_stream,
                inbound_writer,
                shared_id,
                running.clone(),
                bandwidth_cb,
            )
            .map_err(|e| crate::Error::Host(format!("poller register stream: {e}")))?;
    }

    // Register the wait target for runtime kicks.
    let gen_offset = ring_offset + GENERATION_COUNTER_OFFSET;
    runtime
        .network_wait_keys
        .lock()
        .push((shared_id, gen_offset));

    // Spawn the outbound drain on a dedicated thread. Outbound bytes are
    // attributed to the connecting process's bandwidth counter.
    let memory = kernel.memory();
    let rt = runtime.clone();
    let drain_rt = runtime.clone();
    let drain_pid = process_id;
    thread::spawn(move || {
        if let Err(_e) = proxy_outbound_tcp(
            outbound_stream,
            outbound_reader,
            running,
            Some(Box::new(move |bytes: u64| {
                drain_rt.record_bandwidth_usage(drain_pid, bytes);
            })),
        ) {}
        // Cleanup: remove wait key on exit.
        rt.network_wait_keys
            .lock()
            .retain(|(sid, _)| *sid != shared_id);
        drop(memory.detach_shared_region(parent_local_id));
    });

    Ok(descriptor)
}

/// Bind a UDP socket and register it with the mio poller.
pub fn udp_bind(runtime: &Runtime, address: String) -> Result<SharedRegionDescriptor> {
    let kernel = &runtime.kernel;
    let std_socket = UdpSocket::bind(&address)
        .map_err(|e| crate::Error::Host(format!("udp bind failed: {e}")))?;
    std_socket
        .set_nonblocking(true)
        .map_err(|e| crate::Error::Host(format!("udp set_nonblocking failed: {e}")))?;

    let outbound_socket = std_socket
        .try_clone()
        .map_err(|e| crate::Error::Host(format!("udp socket clone failed: {e}")))?;

    let (descriptor, recv_writer, send_reader, ring_offset, parent_local_id) =
        create_stream_region(kernel)?;

    let shared_id = descriptor.shared_id;
    let running = Arc::new(AtomicBool::new(true));

    kernel.network().insert_udp_socket(
        shared_id,
        selium_kernel::UdpSocketState {
            running: running.clone(),
        },
    );

    // Register the socket for inbound recv with the mio poller.
    if let Some(poller) = kernel.poller() {
        poller
            .register_udp_socket(std_socket, recv_writer, shared_id, running.clone())
            .map_err(|e| crate::Error::Host(format!("poller register udp: {e}")))?;
    }

    // Register the wait target for runtime kicks.
    let gen_offset = ring_offset + GENERATION_COUNTER_OFFSET;
    runtime
        .network_wait_keys
        .lock()
        .push((shared_id, gen_offset));

    // Spawn the outbound send drain on a dedicated thread.
    let memory = kernel.memory();
    let rt = runtime.clone();
    thread::spawn(move || {
        if let Err(_e) = proxy_outbound_udp(outbound_socket, send_reader, running) {}
        rt.network_wait_keys
            .lock()
            .retain(|(sid, _)| *sid != shared_id);
        drop(memory.detach_shared_region(parent_local_id));
    });

    Ok(descriptor)
}

fn align_up(value: u32, alignment: u32) -> u32 {
    let rem = value % alignment;
    if rem == 0 {
        value
    } else {
        value + alignment - rem
    }
}

fn create_stream_region(
    kernel: &Kernel,
) -> Result<(SharedRegionDescriptor, RingWriter, RingReader, u64, u64)> {
    let ring_data_cap = DEFAULT_RING_CAPACITY as u32;
    let ring_region_len = selium_memory::RING_HEADER_SIZE as u32 + ring_data_cap;
    let header_size =
        selium_memory::HEADER_ENTRY_OFFSET as u32 + 2 * selium_memory::HEADER_ENTRY_SIZE as u32;
    let total_capacity = align_up(
        header_size + align_up(header_size + ring_region_len, 8) + ring_region_len,
        8,
    );

    let memory = kernel.memory();
    let (shared_id, _len) = memory
        .allocate_shared_region(total_capacity)
        .map_err(|e| crate::Error::Host(e.to_string()))?;

    let parent_backend = memory
        .attach_backend(shared_id)
        .map_err(|e| crate::Error::Host(e.to_string()))?;
    let parent_local_id = parent_backend.local_id();

    let sub_memory_0_offset = align_up(header_size, 8) as u64;
    let sub_memory_1_offset = align_up(sub_memory_0_offset as u32 + ring_region_len, 8) as u64;

    MultiMemoryHeader::write_two_entries(
        &parent_backend,
        0,
        total_capacity as u64,
        [
            (sub_memory_0_offset, ring_region_len as u64),
            (sub_memory_1_offset, ring_region_len as u64),
        ],
    )
    .map_err(|e| crate::Error::Host(e.to_string()))?;

    let inbound_backend = parent_backend
        .sub_region(sub_memory_0_offset, ring_region_len as u64)
        .map_err(|e| crate::Error::Host(e.to_string()))?;
    let outbound_backend = parent_backend
        .sub_region(sub_memory_1_offset, ring_region_len as u64)
        .map_err(|e| crate::Error::Host(e.to_string()))?;

    layout::init_ring(inbound_backend.as_ref()).map_err(|e| crate::Error::Host(e.to_string()))?;
    layout::init_ring(outbound_backend.as_ref()).map_err(|e| crate::Error::Host(e.to_string()))?;

    layout::store_capacity(inbound_backend.as_ref(), DEFAULT_RING_CAPACITY)
        .map_err(|e| crate::Error::Host(e.to_string()))?;
    layout::store_capacity(outbound_backend.as_ref(), DEFAULT_RING_CAPACITY)
        .map_err(|e| crate::Error::Host(e.to_string()))?;

    let inbound_writer = RingWriter::open(inbound_backend, DEFAULT_RING_CAPACITY)
        .map_err(|e| crate::Error::Host(e.to_string()))?;
    inbound_writer
        .increment_writer_count()
        .map_err(|e| crate::Error::Host(e.to_string()))?;

    let outbound_reader = RingReader::open(outbound_backend, DEFAULT_RING_CAPACITY, true)
        .map_err(|e| crate::Error::Host(e.to_string()))?;

    let region = SharedRegionDescriptor {
        shared_id,
        len: total_capacity as u64,
    };
    // Return the outbound ring's sub-memory offset for wait-key computation.
    Ok((
        region,
        inbound_writer,
        outbound_reader,
        sub_memory_1_offset,
        parent_local_id,
    ))
}

/// Outbound drain for TCP: reads frames from the ring, writes to the socket.
/// Blocks on `atomic_wait32` on the ring's generation word, with a bounded
/// timeout backstop.
fn proxy_outbound_tcp(
    mut stream: TcpStream,
    mut reader: RingReader,
    running: Arc<AtomicBool>,
    on_bandwidth: Option<Box<dyn Fn(u64) + Send>>,
) -> Result<()> {
    let backend = reader.backend();
    // A fresh ring has zero writers because the peer has not attached yet.
    // That is not EOF: only treat an empty writer set as end-of-stream once
    // at least one writer has been observed.
    let mut seen_writer = false;

    while running.load(Ordering::Relaxed) {
        // Snapshot the generation *before* draining. The drain loop below
        // must never gate frame delivery on generation, but the value we
        // park on has to be older than any write that lands after our last
        // read: otherwise a write + kick arriving between `read_frame`
        // returning `None` and the generation read would leave us parked on
        // the new generation with the kick already consumed.
        let current_generation = reader
            .generation()
            .map_err(|e| crate::Error::Host(e.to_string()))?;

        // Drain whatever is available first. Generation bookkeeping is only
        // used to decide when to park — never to skip frames — so frames
        // written before this thread started are still delivered.
        let mut saw_frame = false;
        loop {
            match reader.read_frame() {
                Ok(Some((_header, payload))) => {
                    let written = payload.len() as u64;
                    if let Err(_e) = stream.write_all(&payload) {
                        running.store(false, Ordering::Relaxed);
                        return Ok(());
                    }
                    if let Err(_e) = stream.flush() {
                        running.store(false, Ordering::Relaxed);
                        return Ok(());
                    }
                    if let Some(record) = on_bandwidth.as_ref() {
                        record(written);
                    }
                    saw_frame = true;
                }
                Ok(None) => break,
                Err(_) => {
                    running.store(false, Ordering::Relaxed);
                    return Ok(());
                }
            }
        }

        if !saw_frame {
            // Nothing available: check whether writers are still connected.
            match reader.writer_count() {
                Ok(0) => {
                    if seen_writer {
                        drop(stream.shutdown(std::net::Shutdown::Write));
                        break;
                    }
                }
                Ok(n) => seen_writer = n > 0,
                Err(_) => break,
            }

            // Block on the generation word with a bounded timeout. The wait
            // re-checks the word internally, so an advance racing with us
            // here resolves to an immediate wake.
            let gen_low = (current_generation & 0xFFFF_FFFF) as u32;
            drop(backend.atomic_wait32(
                GENERATION_COUNTER_OFFSET,
                gen_low,
                OUTBOUND_WAIT_TIMEOUT_MS,
            ));
        }
    }

    running.store(false, Ordering::Relaxed);
    Ok(())
}

/// Outbound drain for UDP: reads datagram frames from the ring, sends to
/// the socket. Blocks on `atomic_wait32` with bounded timeout.
fn proxy_outbound_udp(
    socket: UdpSocket,
    mut reader: RingReader,
    running: Arc<AtomicBool>,
) -> Result<()> {
    let backend = reader.backend();
    // See proxy_outbound_tcp: zero writers before first attach is not EOF.
    let mut seen_writer = false;

    while running.load(Ordering::Relaxed) {
        // Snapshot the generation *before* draining; see proxy_outbound_tcp
        // for why the value we park on must be older than any write arriving
        // after our last read.
        let current_generation = reader
            .generation()
            .map_err(|e| crate::Error::Host(e.to_string()))?;

        // Drain whatever is available first; see proxy_outbound_tcp for why
        // generation bookkeeping must not gate frame delivery.
        let mut saw_frame = false;
        loop {
            match reader.read_frame() {
                Ok(Some((_header, frame))) => {
                    let (addr, payload) = match selium_kernel::decode_udp_frame(&frame) {
                        Some(d) => (d.0, d.1),
                        None => continue,
                    };
                    // The proxy socket is non-blocking, so `send_to` can fail
                    // transiently. Treating every error as fatal (and exiting
                    // silently) strands every later datagram — a single
                    // transient `WouldBlock` would take down the whole relay.
                    // Drop that one datagram instead: for QUIC, loss recovery
                    // resends it.
                    match socket.send_to(payload, addr) {
                        Ok(_) => {}
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            eprintln!(
                                "outbound UDP send blocked; dropping {}B bound for {addr}",
                                payload.len()
                            );
                        }
                        // EINTR is spurious on UDP; retry the same datagram.
                        Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {
                            if let Err(e) = socket.send_to(payload, addr) {
                                eprintln!(
                                    "outbound UDP send to {addr} failed: {e}; stopping relay"
                                );
                                running.store(false, Ordering::Relaxed);
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            eprintln!("outbound UDP send to {addr} failed: {e}; stopping relay");
                            running.store(false, Ordering::Relaxed);
                            return Ok(());
                        }
                    }
                    saw_frame = true;
                }
                Ok(None) => break,
                Err(_) => {
                    running.store(false, Ordering::Relaxed);
                    return Ok(());
                }
            }
        }

        if !saw_frame {
            match reader.writer_count() {
                Ok(0) => {
                    if seen_writer {
                        break;
                    }
                }
                Ok(n) => seen_writer = n > 0,
                Err(_) => break,
            }

            let gen_low = (current_generation & 0xFFFF_FFFF) as u32;
            drop(backend.atomic_wait32(
                GENERATION_COUNTER_OFFSET,
                gen_low,
                OUTBOUND_WAIT_TIMEOUT_MS,
            ));
        }
    }

    running.store(false, Ordering::Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use selium_kernel::Kernel;
    use selium_memory::MultiMemoryHeader;

    #[tokio::test]
    async fn tcp_bind_creates_host_queue() {
        let kernel = Kernel::default();
        let _ = kernel.init_poller().expect("init poller");
        let runtime = Runtime::new(kernel);
        let descriptor = tcp_bind(&runtime, 1, "127.0.0.1:0".to_string()).expect("tcp bind");
        assert!(descriptor.shared_id > 0);
        assert!(descriptor.local_id > 0);
        runtime
            .kernel
            .close_tcp_listener(descriptor.local_id)
            .unwrap();
    }

    #[tokio::test]
    async fn tcp_connect_returns_shared_region() {
        let kernel = Kernel::default();
        let _ = kernel.init_poller().expect("init poller");
        let runtime = Runtime::new(kernel);
        let helper = std::net::TcpListener::bind("127.0.0.1:0").expect("bind helper");
        let addr = helper.local_addr().expect("helper addr");

        std::thread::spawn(move || {
            drop(helper.accept());
        });

        let descriptor = tcp_connect(&runtime, 0, addr.to_string()).expect("tcp connect");
        assert!(descriptor.shared_id > 0);
        assert!(descriptor.len > 0);

        let memory = runtime.kernel.memory();
        let backend = memory.attach_backend(descriptor.shared_id).expect("attach");
        let header = MultiMemoryHeader::parse(&backend, 0).expect("parse header");
        assert_eq!(header.count, 2, "expected 2 sub-memories");
    }

    #[tokio::test]
    async fn udp_bind_returns_shared_region() {
        let kernel = Kernel::default();
        let _ = kernel.init_poller().expect("init poller");
        let runtime = Runtime::new(kernel);
        let descriptor = udp_bind(&runtime, "127.0.0.1:0".to_string()).expect("udp bind");
        assert!(descriptor.shared_id > 0);
        assert!(descriptor.len > 0);
        runtime
            .kernel
            .close_udp_socket(descriptor.shared_id)
            .unwrap();
    }

    #[tokio::test]
    async fn create_stream_region_has_correct_layout() {
        let kernel = Kernel::default();
        let (region, _inbound_writer, _outbound_reader, _ring_offset, parent_local_id) =
            create_stream_region(&kernel).expect("create stream region");

        let memory = kernel.memory();
        let backend = memory.attach_backend(region.shared_id).expect("attach");
        let header = MultiMemoryHeader::parse(&backend, 0).expect("parse");
        assert_eq!(header.count, 2);

        drop(memory.detach_shared_region(backend.local_id()));
        drop(memory.detach_shared_region(parent_local_id));
    }

    /// Verify no sleep-based polling remains in network proxy paths.
    /// The source of `network.rs` must not contain `thread::sleep` or
    /// `tokio::time::sleep` outside of test-only code.
    #[test]
    fn proxy_paths_do_not_spin() {
        let source = include_str!("network.rs");
        // Only examine production code (before #[cfg(test)]).
        let production = match source.split_once("#[cfg(test)]") {
            Some((prod, _tests)) => prod,
            None => source,
        };
        assert!(
            !production.contains("thread::sleep"),
            "network proxy paths must not use thread::sleep; use mio poller and atomic_wait32 instead"
        );
        assert!(
            !production.contains("tokio::time::sleep"),
            "network proxy paths must not use tokio::time::sleep; use mio poller and atomic_wait32 instead"
        );
    }

    /// End-to-end TCP echo test using the event-driven proxy infrastructure.
    #[tokio::test]
    async fn tcp_echo_via_event_driven_proxy() {
        use selium_memory::MultiMemoryHeader;
        use std::io::{Read, Write};

        let kernel = Kernel::default();
        let _ = kernel.init_poller().expect("init poller");
        let runtime = Runtime::new(kernel.clone());

        // Start a simple echo server.
        let server = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let server_addr = server.local_addr().expect("addr");

        let server_handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = server.accept() {
                let mut buf = [0u8; 256];
                if let Ok(n) = stream.read(&mut buf) {
                    drop(stream.write_all(&buf[..n]));
                    drop(stream.flush());
                }
            }
        });

        // Connect via the event-driven proxy.
        let descriptor = tcp_connect(&runtime, 0, server_addr.to_string()).expect("tcp connect");
        let shared_id = descriptor.shared_id;

        // Write to the outbound ring (simulating a guest write).
        let memory = runtime.kernel.memory();
        let parent_backend = memory.attach_backend(shared_id).expect("attach");
        let header = MultiMemoryHeader::parse(&parent_backend, 0).expect("parse");
        let outbound_entry = header.entry(1).expect("outbound entry");

        let outbound_backend = parent_backend
            .sub_region(outbound_entry.offset, outbound_entry.length)
            .expect("outbound sub");
        let outbound_writer =
            RingWriter::open(outbound_backend, DEFAULT_RING_CAPACITY).expect("open writer");
        outbound_writer.increment_writer_count().expect("inc wc");
        outbound_writer
            .write_frame(b"hello event-driven", 0, 0)
            .expect("write frame");

        // In production, the guest reactor would stall or make a hostcall after
        // writing, which triggers kick_network_waiters. In this test, we
        // simulate that kick manually.
        runtime.kick_network_waiters();

        // Read response from the inbound ring (simulating a guest read).
        let inbound_entry = header.entry(0).expect("inbound entry");
        let inbound_backend = parent_backend
            .sub_region(inbound_entry.offset, inbound_entry.length)
            .expect("inbound sub");
        let mut inbound_reader =
            RingReader::open(inbound_backend, DEFAULT_RING_CAPACITY, false).expect("open reader");

        // Wait for the echo response. Since the poller pumps data
        // on socket readable events, it should arrive without sleep polling.
        let start = std::time::Instant::now();
        let mut found = false;
        while start.elapsed() < std::time::Duration::from_secs(5) {
            if let Ok(Some((_h, payload))) = inbound_reader.read_frame()
                && payload == b"hello event-driven"
            {
                found = true;
                break;
            }
            // Yield to let the poller thread run.
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert!(
            found,
            "expected echo response on inbound ring within timeout"
        );

        // Verify the response arrived without relying on the 1-second backstop
        // timeout — the poller should handle this in well under 1 second.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "wake latency should be under 1 second, was {:?}",
            start.elapsed()
        );

        drop(outbound_writer);
        server_handle.join().expect("server thread");
        runtime.kernel.close_tcp_stream(shared_id).unwrap();
    }

    /// Stage 1 scenario: a guest write wakes the outbound drainer through the
    /// unified region waiter registry with **no runtime kick**. The
    /// `notify_region` call here stands in for a fast-path guest's
    /// `memory.atomic.notify` landing on the same registry the drainer parks
    /// in, with `kick_network_waiters` never invoked.
    #[tokio::test]
    async fn tcp_echo_via_unified_registry_no_kick() {
        use std::io::{Read, Write};

        let kernel = Kernel::default();
        let _ = kernel.init_poller().expect("init poller");
        let runtime = Runtime::new(kernel.clone());

        let server = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let server_addr = server.local_addr().expect("addr");
        let server_handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = server.accept() {
                let mut buf = [0u8; 256];
                if let Ok(n) = stream.read(&mut buf) {
                    drop(stream.write_all(&buf[..n]));
                    drop(stream.flush());
                }
            }
        });

        let descriptor = tcp_connect(&runtime, 0, server_addr.to_string()).expect("tcp connect");
        let shared_id = descriptor.shared_id;

        let memory = runtime.kernel.memory();
        let parent_backend = memory.attach_backend(shared_id).expect("attach");
        let header = MultiMemoryHeader::parse(&parent_backend, 0).expect("parse");
        let outbound_entry = header.entry(1).expect("outbound entry");

        let outbound_backend = parent_backend
            .sub_region(outbound_entry.offset, outbound_entry.length)
            .expect("outbound sub");
        let outbound_writer =
            RingWriter::open(outbound_backend, DEFAULT_RING_CAPACITY).expect("open writer");
        outbound_writer.increment_writer_count().expect("inc wc");
        outbound_writer
            .write_frame(b"registry wake", 0, 0)
            .expect("write frame");

        // Notably absent: `kick_network_waiters()`. The fast-path guest's
        // generation-word notify reaches the drainer directly through the
        // unified registry, exactly like a `memory.atomic.notify` would.
        memory
            .notify_region(shared_id, outbound_entry.offset, 1)
            .expect("notify region");

        let inbound_entry = header.entry(0).expect("inbound entry");
        let inbound_backend = parent_backend
            .sub_region(inbound_entry.offset, inbound_entry.length)
            .expect("inbound sub");
        let mut inbound_reader =
            RingReader::open(inbound_backend, DEFAULT_RING_CAPACITY, false).expect("open reader");

        let start = std::time::Instant::now();
        let mut found = false;
        while start.elapsed() < std::time::Duration::from_secs(5) {
            if let Ok(Some((_h, payload))) = inbound_reader.read_frame()
                && payload == b"registry wake"
            {
                found = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert!(
            found,
            "expected echo via the unified registry with no runtime kick"
        );

        drop(outbound_writer);
        server_handle.join().expect("server thread");
        runtime.kernel.close_tcp_stream(shared_id).unwrap();
    }

    /// EOF propagation: when the remote closes, the reader sees writer_count=0.
    #[tokio::test]
    async fn eof_propagation_on_remote_close() {
        use selium_memory::MultiMemoryHeader;

        let kernel = Kernel::default();
        let _ = kernel.init_poller().expect("init poller");
        let runtime = Runtime::new(kernel.clone());

        let server = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let server_addr = server.local_addr().expect("addr");

        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let server_handle = std::thread::spawn(move || {
            if let Ok((stream, _)) = server.accept() {
                ready_tx.send(()).expect("signal ready");
                // Immediately close the connection (EOF without data).
                drop(stream);
            }
        });

        let descriptor = tcp_connect(&runtime, 0, server_addr.to_string()).expect("tcp connect");
        let shared_id = descriptor.shared_id;

        // Wait for server to accept and close.
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("server accepted");

        // Read from the inbound ring — the poller should detect EOF and
        // decrement the writer count.
        let memory = runtime.kernel.memory();
        let parent_backend = memory.attach_backend(shared_id).expect("attach");
        let header = MultiMemoryHeader::parse(&parent_backend, 0).expect("parse");
        let inbound_entry = header.entry(0).expect("inbound entry");
        let inbound_backend = parent_backend
            .sub_region(inbound_entry.offset, inbound_entry.length)
            .expect("inbound sub");
        let inbound_reader =
            RingReader::open(inbound_backend.clone(), DEFAULT_RING_CAPACITY, false)
                .expect("open reader");

        // Poll until writer_count reaches 0 (EOF).
        let start = std::time::Instant::now();
        let mut eof = false;
        while start.elapsed() < std::time::Duration::from_secs(5) {
            if let Ok(0) = inbound_reader.writer_count() {
                eof = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert!(eof, "EOF should propagate within timeout");

        server_handle.join().expect("server thread");
        runtime.kernel.close_tcp_stream(shared_id).unwrap();
    }
}
