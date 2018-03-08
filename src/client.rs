use std::net::{ SocketAddr, ToSocketAddrs };
use bit_set::BitSet;
use std::fs::File;
use std::io::{ self, Read, Write, Seek };
use std::path::Path;
use futures::{ Future, Poll, Async };
use std::net::UdpSocket;
use std::time::Duration;
use std::sync::{ Arc, Mutex };
use memmap::{ Mmap, MmapOptions };
use std::time::Instant;

use net_util::LOCAL_IP;
use types::*;
use header::*;

const MAX_ATTEMPTS: u32 = 4;

/// Represents what action a TFTPClient is currently performing. By default, a TFTPClient will have
/// the Action::NoAction.
#[derive(Clone, Debug)]
pub enum Action {
    NoAction,
    SendFile,
    RequestFile,
    SendError,
    EstablishConnection
}

pub struct TFTPClient {
    host_address: SocketAddr,
    action: Action,
    local_port: u16,
    udp_socket: Arc<Mutex<UdpSocket>>
}

impl TFTPClient {
    pub fn new<T: ToSocketAddrs>(host_address: T, local_port: u16) -> Result<Self, Option<io::Error>> {
        let local_socket_addr: SocketAddr = SocketAddr::from((LOCAL_IP.clone(), local_port));
        let udp_socket: UdpSocket;
        let res = UdpSocket::bind(local_socket_addr);
        match res {
            Ok(socket) => udp_socket = socket,
            Err(e) => return Err(Some(e))
        };

        let addr = host_address.to_socket_addrs();
        if let Err(e) = addr {
            return Err(Some(e))
        }
        let address_opt = addr.unwrap().next();
        if let Some(host_address) = address_opt {
            Ok(TFTPClient {
                action: Action::NoAction,
                host_address,
                local_port,
                udp_socket: Arc::new(Mutex::new(udp_socket))
            })
        } else {
            Err(None)
        }
    }

    //fn connect_to_host(host_address: SocketAddr) -> impl Future<Item=(), Error=io::Error> { unimplemented!() }
    //pub fn send_file<P: AsRef<Path>, S: AsRef<Path>>(source: P, filename: S) -> impl Future<Item=i32, Error=io::Error> { unimplemented!() }
    //pub fn request_file<P: AsRef<Path>, S: AsRef<Path>>(filename: P, destination: S) -> impl Future<Item=i32, Error=io::Error> { unimplemented!() }
    pub fn send_error(&mut self, error: ErrorCode) -> impl Future<Item=(), Error=io::Error> {
        SendError::new(ErrorHeader::new(error, "1".to_string()).unwrap(), self.host_address.clone(), self.udp_socket.clone())
    }

    /*
    pub fn send_data(&mut self, data: &[u8], block_number: u32) -> Option<impl Future<Item=u32, Error=io::Error>> {
        SendData::new(data, block_number, self.host_address.clone(), self.udp_socket.clone())
    }
    */
}

const WINDOW_SIZE: usize = 16;

pub struct SendFile {
    /// A file backed buffer, allows the file to be indexed like an array!
    file_map: Mmap,

    /// The UDP socket to send data through
    socket: Arc<Mutex<UdpSocket>>,

    /// The host address to send data to
    host_addr: SocketAddr,

    /// Blocks that are awaiting Acks
    blocks_pending_acks: BitSet,

    /// Blocks that need to be sent again because an Ack was not received.
    blocks_to_repeat: BitSet,

    /// A set of all blocks that Acks have been received for.
    sent_blocks: BitSet,

    /// The total number of blocks in the file.
    num_blocks: usize,

    /// The next block index to send.
    next_to_send: usize,

    /// The current window!
    current_window: usize,

    /// The time the current window started
    window_start: Instant,

    /// Holds a SendData object if it has not successfully been sent yet.
    try_again: Option<SendData>,

    /// A list of all of times the blocks in the current window were sent. None if it hasn't been sent yet.
    block_times: [Option<Instant>; WINDOW_SIZE],

    /// The exponential moving average of the round trip time
    average_rtt: Duration
}

impl SendFile {
    pub fn new(file: &File, socket: Arc<Mutex<UdpSocket>>, host_addr: SocketAddr) -> Result<Self, io::Error> {
        let file_map = unsafe { MmapOptions::new().map(file)? };
        let num_blocks: usize = file_map.len()?;
        if num_blocks > (1 << 24) - 1 { return Err(io::Error::new(io::ErrorKind::Other, "Files greater than 8GB in size cannot be sent.")) }
        // The number of whole blocks, plus another block if there is extra
        let num_blocks: usize = num_blocks / MAX_DATA_LEN + (if num_blocks & (MAX_DATA_LEN - 1) > 0 { 1 } else { 0 });

        let mut sent_blocks = BitSet::with_capacity(num_blocks);

        Ok(SendFile {
            file_map,
            socket,
            host_addr,
            sent_blocks: BitSet::with_capacity(num_blocks),
            blocks_pending_acks: BitSet::with_capacity(num_blocks),
            blocks_to_repeat: BitSet::with_capacity(num_blocks),
            num_blocks,
            next_to_send: 0,
            try_again: None,
            current_window: 0,
            window_start: Instant::now(),
            block_times: [None; WINDOW_SIZE],
            average_rtt: Duration::from_secs(1)
        })
    }

    pub fn num_windows(&self) -> usize {
        self.num_blocks / 16 + (if self.num_blocks & 15 == 0 { 0 } else { 1 })
    }

    pub fn get_block_n(&self, block_number: usize) -> Option<SendData> {
        SendData::new(data, block_number, self.host_addr.clone(), self.socket.clone())
    }

    /// Will continue to return Some(..) until the current window has reached its end, or there is no more data to send.
    pub fn next_block(&mut self) -> Option<SendData> {
        // If we haven't sent any of the blocks in the current window
        if self.next_to_send < WINDOW_SIZE * self.current_window {
            let send_data = self.get_block_n(self.next_to_send);
            match send_data {
                send_data @ Some(_) => {
                    self.next_to_send += 1;
                    send_data
                },
                none @ None => { none }
            }
        } else {
        // We've sent all of the blocks in the current window, if they've all received Acks, lets
        // move to the next window; otherwise, wait until we have received acks for all of them.
            let window_base = WINDOW_SIZE * self.current_window;
            for i in window_base..window_base + WINDOW_SIZE {
                if self.blocks_pending_acks.contains(i) {
                    let send_time: Instant = self.block_times[i & (WIDOW_SIZE - 1)].unwrap();
                    if send_time.elapsed() > self.average_rtt.mul(2) {
                        self.blocks_pending_acks.remove(i);
                        return Some(self.get_block_n(i))
                    }
                }
            }
            self.window_start = Instant::now();
            self.current_window += 1;

            // If currentwindow < self.num_blocks / WINDOW_SIZE that means the self.current_window
            // is actually a valid window
            if self.current_window < self.num_blocks / WINDOW_SIZE {
                self.next_block()
            } else {
                None
            }
        }
    }

    fn send_data(&mut self, mut to_send: SendData) -> Poll<(), io::Error> {
        match to_send.poll() {
                Ok(Async::Ready(block_number)) => {
                    self.blocks_pending_acks.insert(block_number);
                    self.block_times[i] = Some(Instant::now());
                    Ok(Async::NotReady)
                },
                // Failed to send again... There is a maximum number of times that a packet can be sent so try it again.
                Ok(Async::NotReady) => {
                    if to_send.send_attempts < MAX_ATTEMPTS {
                        self.try_again = Some(to_send);
                        Ok(Async::NotReady)
                    } else {
                        Err(io::Error::new(io::ErrorKind::Other, "Failed to send packet too many times consecutively."))
                    }
                },
                Err(e) => Err(e)
        }
    }

    fn update_average_rtt(&mut self, rtt: Duration) {
        // hopefully this will be compiles and optimized to 5 bit shifts and one subtract op.
        self.average_rtt = rtt.div(16) + self.average_rtt.mul(15).div(16);
    }
}

impl Future for SendFile {
    type Item = ();
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {

        if let Some(to_send) = self.try_again.take() {
            // Try to send this data again, if it succesfully gets sent, add it to the blocks_pending_acks set
            self.send_data(to_send)
        } else if let Some(send_data) = self.next_block() {
            // try to send this data, if it successfully gets sent, add it to the blocks_pending_acks set
            self.send_data(to_send)
        } else {
            // we've sent all of our packets in this window already, but haven't received acks for everything.
            // If it has been a reasonable amount of time, add everything to a "to-resend" set,
            // and send all of them. Also, reset the window_start so this code wont execute unless it
            // has been a ong enough time to justify another resend.

            let no_blocks_pending = self.blocks_pending_acks.is_empty();
            let no_blocks_to_repeat = self.blocks_to_repeat.is_empty();
            // Check if we're done!
            if no_blocks_pending && no_blocks_to_repeat && self.blocks_received_acks.len() == self.num_blocks {
                return Ok(Async::Ready(()))
            }

            let blocks_p
        }
    }
}

pub struct SendData {
    /// The encoded header
    raw_header: RawRequest,

    send_attempts: u32,

    /// UDP Socket handle
    socket: Arc<Mutex<UdpSocket>>,

    host_addr: SocketAddr,

    block_number: usize
}

impl SendData {
    pub fn new(data: &[u8], block_number: usize, host_addr: SocketAddr, socket: Arc<Mutex<UdpSocket>>) -> Option<SendData> {
        let data_header = DataHeader::new(data, block_number);
        if data_header.is_none() { return None }
        let data_header = data_header.unwrap();
        Some(SendData { raw_header: data_header.into(), send_attempts: 0, block_number, socket, host_addr })
    }
}

impl Future for SendData {
    type Item = usize;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let mut lock = self.socket.try_lock();
        if let Ok(ref mut socket) = lock {
            match (*socket).send_to(self.raw_header.as_ref(), self.host_addr) {
                Ok(bytes_written) => {
                    if bytes_written != self.raw_header.len() {
                        Err(io::Error::new(io::ErrorKind::Other, "Failed to send all data in one UDP packet."))
                    } else {
                        Ok(Async::Ready(self.block_number))
                    }
                },
                Err(e) => {
                    self.send_attempts += 1;
                    if self.send_attempts > MAX_ATTEMPTS {
                        Err(e)
                    } else {
                        Ok(Async::NotReady)
                    }
                }
            }
        } else {
            Ok(Async::NotReady)
        }
    }
}

pub struct SendError {
    host_addr: SocketAddr,
    socket: Arc<Mutex<UdpSocket>>,
    send_attempts: usize,
    raw_header: RawRequest
}

impl SendError {
    pub fn new(error: ErrorHeader, host_addr: SocketAddr, socket: Arc<Mutex<UdpSocket>>) -> SendError {
        SendError { host_addr, socket, send_attempts: 0, raw_header: error.into() }
    }
}

impl Future for SendError {
    type Item = ();
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let mut lock = self.socket.try_lock();
        if let Ok(ref mut socket) = lock {
            match (*socket).send_to(self.raw_header.as_ref(), self.host_addr) {
                Ok(bytes_written) => {
                    if bytes_written != self.raw_header.len() {
                        Err(io::Error::new(io::ErrorKind::Other, "Failed to send all data in one UDP packet."))
                    } else {
                        Ok(Async::Ready(()))
                    }
                },
                Err(e) => {
                    self.send_attempts += 1;
                    if self.send_attempts > MAX_ATTEMPTS {
                        Err(e)
                    } else {
                        Ok(Async::NotReady)
                    }
                }
            }
        } else {
            Ok(Async::NotReady)
        }
    }
}
