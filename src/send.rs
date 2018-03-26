use std::net::{ SocketAddr, ToSocketAddrs };
use bit_set::BitSet;
use bit_vec::BitVec;
use std::fs::File;
use std::io::{ self, Seek };
use futures::{ Future, Poll, Async };
use std::net::UdpSocket;
use std::time::Duration;
use std::sync::{ Arc, Mutex };
use memmap::{ Mmap, MmapOptions };
use std::time::Instant;
use std::collections::{ BinaryHeap, HashMap };
use error::TFTPError;
use std::ops::*;
use std::cmp::*;
use header::*;
use client::*;

pub const WINDOW_SIZE: usize = 16;

#[derive(Clone)]
struct BlockData {
    pub time_sent: Instant,
    pub block_number: usize
}

impl PartialEq<BlockData> for BlockData {
    fn eq(&self, other: &BlockData) -> bool {
        self.time_sent == other.time_sent && self.block_number == other.block_number
    }
}

impl Eq for BlockData {}

impl PartialOrd<BlockData> for BlockData {
    fn partial_cmp(&self, other: &BlockData) -> Option<Ordering> {
        self.time_sent.partial_cmp(&other.time_sent)
    }
}

impl Ord for BlockData {
    fn cmp(&self, other: &Self) -> Ordering {
        self.time_sent.cmp(&other.time_sent)
    }
}

pub struct SendFile {
    /// The file!
    file: File,

    /// A file backed buffer, allows the file to be indexed like an array!
    file_map: Mmap,

    /// The exact length, in bytes, of file_map
    file_len: usize,

    /// The UDP socket to send data through
    socket: Arc<Mutex<UdpSocket>>,

    /// The host address to send data to
    host_addr: SocketAddr,

    /// Blocks that are awaiting Acks. This includes blocks that haven't actually been sent yet!
    blocks_pending_acks: BitSet,

    /// The total number of blocks in the file.
    num_blocks: usize,

    /// The next block index to send.
    next_to_send: usize,

    /// Holds a SendData object if it has not successfully been sent yet.
    try_again: Option<SendData>,

    /// A priority queue that contains instances at which blocks were sent.
    timeout_pq: BinaryHeap<BlockData>,

    /// For all blocks that have been sent and have not yet received an Ack, this hashmap contains
    /// the time at which it was sent. This is in done to allow the calculation of [average_rtt]
    send_times: HashMap<usize, Instant>,

    /// The exponential moving average of the round trip time
    average_rtt: Duration
}

impl SendFile {
    pub fn new(file: File, socket: Arc<Mutex<UdpSocket>>, host_addr: SocketAddr) -> Result<Self, io::Error> {
        let file_map = unsafe { MmapOptions::new().map(&file)? };
        let file_len: usize = file_map.len();
        if file_len > (1 << 24) * MAX_DATA_LEN { return Err(io::Error::new(io::ErrorKind::Other, "Files greater than 8GB in size cannot be sent.")) }
        // The number of whole blocks, plus another block if there is extra
        let num_blocks: usize = file_len / MAX_DATA_LEN + (if file_len & (MAX_DATA_LEN - 1) > 0 { 1 } else { 0 });

        let mut r = SendFile {
            file,
            file_map,
            file_len,
            socket,
            host_addr,
            num_blocks,
            blocks_pending_acks: BitSet::from_bit_vec(BitVec::from_elem(num_blocks, true)),
            next_to_send: 0,
            try_again: None,
            timeout_pq: BinaryHeap::new(),
            send_times: HashMap::with_capacity(WINDOW_SIZE),
            average_rtt: Duration::from_secs(1)
        };
        r.init()
    }

    fn init(mut self) -> Result<Self, io::Error> {
        // Receive an Ack for the write request... Try several times to receive an Ack
        for i in 0..MAX_ATTEMPTS {
            match self.receive_header() {
                Ok(Some(Header::Ack(ack))) => break,
                _ => {
                    if i == MAX_ATTEMPTS - 1 {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "Did not receive an ACK for the write request."))
                    } else {
                        continue
                    }
                }
            }
        }

        for _ in 0..WINDOW_SIZE {
            if let Some(block) = self.next_block() {
                self.send_data(block)?;
            } else {
                break
            }
        }
        Ok(self)
    }

    pub fn run(mut self) -> Result<(), io::Error> {
        loop {
            let r = self.poll();
            match r {
                Ok(Async::NotReady) => continue,
                Ok(Async::Ready(())) => return Ok(()),
                Err(e) => return Err(e)
            }
        }
    }

    pub fn get_block_n(&self, block_number: usize) -> Option<SendData> {
        if block_number >= self.num_blocks { return None }

        let mut data = vec![0u8; MAX_DATA_LEN];
        if block_number == self.num_blocks - 1 {
            data[0..(self.file_len - block_number * MAX_DATA_LEN)]
                .clone_from_slice(&self.file_map[block_number * MAX_DATA_LEN .. self.file_len]);
        } else {
            data[..].clone_from_slice(&self.file_map[block_number * MAX_DATA_LEN..block_number * (MAX_DATA_LEN) + MAX_DATA_LEN]);
        }
        SendData::new(&data, block_number, self.host_addr.clone(), self.socket.clone())
    }

    /// Will continue to return Some(..) until the current window has reached its end, or there is no more data to send.
    pub fn next_block(&mut self) -> Option<SendData> {
        let next_block = self.next_to_send;
        self.next_to_send += 1;
        if self.next_to_send < self.num_blocks {
            Some(self.get_block_n(next_block).unwrap())
        } else if self.next_to_send == self.num_blocks {
            Some(SendData::new_empty(next_block, self.host_addr.clone(), self.socket.clone()))
        } else {
            None
        }
    }

    fn send_data(&mut self, mut to_send: SendData) -> Poll<(), io::Error> {
        let time_sent = Instant::now();
        match to_send.poll() {
            Ok(Async::Ready(block_number)) => {
                self.blocks_pending_acks.insert(block_number);
                self.timeout_pq.push(BlockData { time_sent: time_sent.clone(), block_number });
                *self.send_times.entry(block_number).or_insert(time_sent) = time_sent;
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

    fn handle_ack(&mut self, ack_header: AckHeader) -> Poll<(), io::Error> {
        self.blocks_pending_acks.remove(ack_header.block_number as usize);
        if let Some(instant) = self.send_times.remove(&(ack_header.block_number as usize)) {
            self.update_average_rtt(instant.elapsed());
        } else {
            // This is the second Ack received for this block! No big deal
        }
        Ok(Async::NotReady)
    }

    fn send_next_block(&mut self) -> Result<(), io::Error> {
        if let Some(next_block) = self.next_block() {
            self.send_data(next_block)?;
            Ok(())
        } else {
            Ok(())
        }
    }

    fn get_timeout(&mut self) -> Option<SendData> {
        loop {
            if !self.timeout_pq.is_empty() {
                let possible_timeout = self.timeout_pq.peek().unwrap().clone();
                if self.blocks_pending_acks.contains(possible_timeout.block_number) {
                    // We're still awaiting an ack from this block and it has not gone over the
                    // timeout threshold yet, and since the priorityqueue is sorted we know all
                    // subsequent entries haven't surpassed the timeout either. So return none
                    if possible_timeout.time_sent.elapsed() < self.average_rtt.mul(3) {
                        return None
                    }
                    break;
                } else {
                    // We've already received an ack for this, remove it from the pq
                    let _ = self.timeout_pq.pop();
                }
            } else {
                return None
            }
        }
        let block_number = self.timeout_pq.pop().unwrap().block_number;
        return self.get_block_n(block_number);
    }

    fn handle_error(&mut self, err_header: ErrorHeader) -> Poll<(), io::Error> {
        Err(io::Error::new(io::ErrorKind::Other, err_header.error_message))
    }

    fn receive_header(&mut self) -> Result<Option<Header>, io::Error> {
        if let Ok(ref mut socket) = self.socket.try_lock() {
            match Header::recv(self.host_addr.clone(), socket) {
                Ok(r)   => Ok(Some(r)),
                Err(e)  => {
                    if let TFTPError::IOError(ioerr) = e {
                        Err(ioerr)
                    } else {
                        Ok(None)
                    }
                }
            }
        } else {
            Ok(None)
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

        // Failed to send this data the first time; try it again!
        if let Some(to_send) = self.try_again.take() {
            // Try to send this data again, if it succesfully gets sent, add it to the blocks_pending_acks set
            let _ = self.send_data(to_send)?;
        }

        if self.next_to_send > self.num_blocks {
            // Done sending everything!
            if self.blocks_pending_acks.is_empty() && self.try_again.is_none() {
                Ok(Async::Ready(()))
            } else {
                // There are still timeouts to handle.
                if let Some(x) = self.timeout_pq.pop() {
                    if self.blocks_pending_acks.contains(x.block_number) {
                        let data: SendData = self.get_block_n(x.block_number).unwrap();
                        self.send_data(data)?;
                    }
                }
                Ok(Async::NotReady)
            }
        } else {

            match self.receive_header() {
                /* // Just ignore the things we don't need instead of giving up.
                Ok(Some(Header::Invalid(_invalid))) =>
                    Err(io::Error::new(io::ErrorKind::InvalidData,
                                              "Unexpectedly received an invalid TFTP header.")),

                Ok(Some(Header::Read(_read_header))) =>
                    Err(io::Error::new(io::ErrorKind::InvalidData,
                                              "Unexpectedly received a TFTP read header.")),

                Ok(Some(Header::Write(_write_header))) =>
                    Err(io::Error::new(io::ErrorKind::InvalidData,
                                              "Unexpectedly received a TFTP write header.")),

                Ok(Some(Header::Data(_data_header))) =>
                    Err(io::Error::new(io::ErrorKind::InvalidData,
                                              "Unexpectedly received a TFTP data header.")),
                */
                Ok(Some(Header::Ack(ack_header))) => {
                    self.handle_ack(ack_header)?;
                    self.send_next_block()?;
                    if let Some(data) = self.get_timeout() { self.send_data(data)?; }
                    Ok(Async::NotReady)
                },

                Ok(Some(Header::Error(err_header))) => self.handle_error(err_header),

                // This means either a header type we don't want was received, or a tftp error occured
                // (respectively).
                Ok(Some(_)) | Ok(None) => Ok(Async::NotReady),

                Err(e) => {
                    // Some I/O errors will be treated as unrecoverable for now.
                    // In the event of repeated errors, Err(..) will be returned by [self.send_next_block]
                    use std::io::ErrorKind::*;
                    match e.kind() {
                        ConnectionRefused | ConnectionReset | ConnectionAborted | NotConnected
                        | AddrInUse | AddrNotAvailable | BrokenPipe | AlreadyExists | InvalidInput |
                        InvalidData | Interrupted | UnexpectedEof => {
                            eprintln!("Encountered non-recoverable I/O error: {:?}", e);
                            Err(e)
                        },
                        _ => Ok(Async::NotReady)
                    }
                }
            }
        }
    }
}

