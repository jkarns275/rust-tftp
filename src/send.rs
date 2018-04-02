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

pub const MAX_WINDOW_SIZE: usize = 256;

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

    /// Window size
    window_size: usize,

    /// the current window range
    ///  lower bound (first) is inclusive, upper bound is exclusive
    window_range: (usize, usize),

    /// The number of consecutive errors that have occured...
    err_counter: usize,

    /// For all blocks that have been sent and have not yet received an Ack, this hashmap contains
    /// the time at which it was sent. This is in done to allow the calculation of [average_rtt]
    send_times: HashMap<usize, Instant>,

    /// The exponential moving average of the round trip time
    average_rtt: Duration,

    /// The number of consecutive timeouts encountered
    timeouts: usize,
}

impl SendFile {
    pub fn new(socket: Arc<Mutex<UdpSocket>>, host_addr: SocketAddr, file: File, window_size: usize) -> Result<Self, io::Error> {
	if window_size <= 1 { unsafe { STOP_AND_WAIT = true } }
        let file_map = unsafe { MmapOptions::new().map(&file)? };
        let file_len: usize = file_map.len();
        if file_len > (1 << 24) * MAX_DATA_LEN { return Err(io::Error::new(io::ErrorKind::Other, "Files greater than 8GB in size cannot be sent.")) }
        // The number of whole blocks, plus another block if there is extra
        let num_blocks: usize = file_len / MAX_DATA_LEN + (if file_len & (MAX_DATA_LEN - 1) == 0 { 0 } else { 1 });
	let window_size = if window_size <= 1 { 1 } else { 2 };
        let mut r = SendFile {
            file,
            file_map,
            file_len,
            socket,
            host_addr,
            num_blocks,
            window_size: window_size,
            err_counter: 0,
            window_range: (0, window_size),
            blocks_pending_acks: BitSet::from_bit_vec(BitVec::from_elem(num_blocks, true)),
            send_times: HashMap::with_capacity(window_size),
            average_rtt: Duration::from_secs(1),
            timeouts: 0
        };
        r.init(window_size)
    }

    // TODO: Fix this when done
    pub fn new_server(socket: Arc<Mutex<UdpSocket>>, host_addr: SocketAddr, file: File, window_size: usize) -> Result<Self, io::Error> {
	if window_size <= 1 { unsafe { STOP_AND_WAIT = true } }
        let file_map = unsafe { MmapOptions::new().map(&file)? };
        let file_len: usize = file_map.len();
        if file_len > (1 << 24) * MAX_DATA_LEN { return Err(io::Error::new(io::ErrorKind::Other, "Files greater than 8GB in size cannot be sent.")) }
        // The number of whole blocks, plus another block if there is extra
        let num_blocks: usize = file_len / MAX_DATA_LEN + (if file_len & (MAX_DATA_LEN - 1) == 0 { 0 } else { 1 });
	let window_size = if window_size <= 1 { 1 } else { 2 };
        let mut r = SendFile {
            file,
            file_map,
            file_len,
            socket,
            host_addr,
            num_blocks,
            window_size: window_size,
            err_counter: 0,
            window_range: (0, window_size),
            blocks_pending_acks: BitSet::from_bit_vec(BitVec::from_elem(num_blocks, true)),
            send_times: HashMap::with_capacity(window_size),
            average_rtt: Duration::from_secs(1),
            timeouts: 0
        };
       
        r.server_init(window_size) 
    }

    fn server_init(mut self, window_size: usize) -> Result<Self, io::Error> {
        let mut a = Header::Ack(AckHeader::new(0));
        if let Ok(ref mut s) = self.socket.try_lock() {
            s.set_read_timeout(Some(self.average_rtt.mul(2)))?;
            match a.send(self.host_addr.clone(), s) {
                Ok(()) => {},
                Err(e) => return Err(e)
            }
        } else { unreachable!()}
        self.send_window()?; 
        Ok(self)
    }

    fn init(mut self, window_size: usize) -> Result<Self, io::Error> {
        // Receive an Ack for the write request... Try several times to receive an Ack
        match self.receive_header() {
            Ok(Some(Header::Ack(ack))) => { /* cool */ },
            _ =>return Err(io::Error::new(io::ErrorKind::InvalidData, "Did not receive an ACK for the write request."))
        }
        self.send_window()?;

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

        let mut data = [0u8; MAX_DATA_LEN];
        if block_number == self.num_blocks - 1 {
            let tail_len = self.file_len - block_number * MAX_DATA_LEN;
            data[0..tail_len]
                .clone_from_slice(&self.file_map[block_number * MAX_DATA_LEN .. self.file_len]);
            SendData::new(&data[0..(self.file_len - block_number * MAX_DATA_LEN)], block_number, self.host_addr.clone(), self.socket.clone())
        } else {
            data[..].clone_from_slice(&self.file_map[block_number * MAX_DATA_LEN..block_number * (MAX_DATA_LEN) + MAX_DATA_LEN]);
            SendData::new(&data, block_number, self.host_addr.clone(), self.socket.clone())
        }
    }

    fn send_data(&mut self, mut to_send: SendData) -> Result<(), io::Error> {
        let time_sent = Instant::now();
        match to_send.poll() {
            Ok(Async::Ready(block_number)) => {
                *self.send_times.entry(block_number).or_insert(time_sent) = time_sent;
                Ok(())
            },
            // Failed to send again... There is a maximum number of times that a packet can be sent so try it again.
            Ok(Async::NotReady) => {
                if to_send.send_attempts < MAX_ATTEMPTS {
                    Ok(())
                } else {
                    Err(io::Error::new(io::ErrorKind::Other, "Failed to send packet too many times consecutively."))
                }
            },
            Err(e) => Err(e)
        }
    }

    fn handle_ack(&mut self, ack_header: AckHeader) -> Poll<(), io::Error> {
        if ack_header.block_number < self.window_range.0 {
		for i in ack_header.block_number + 1..self.window_range.0 {
			self.blocks_pending_acks.insert(i);
		}
		self.window_range.0 = ack_header.block_number + 1;
		self.window_range.1 = self.window_range.0 + self.window_size;
	} else {
        
        // If the whole window we sent last time was received, increase it!
        if !unsafe { STOP_AND_WAIT } { 
	if ack_header.block_number + 1 == self.window_range.1 {
    	    self.window_size <<= 1;
            if self.window_size == 0 { self.window_size == 1; }
            else if self.window_size > MAX_WINDOW_SIZE { self.window_size = MAX_WINDOW_SIZE; }
        } else { // otherwise make it smaller..
            self.window_size >>= 1;
            if self.window_size == 0 { self.window_size == 1; }
        }}
	}

        for block_number in self.window_range.0..=(ack_header.block_number as usize) {
            self.blocks_pending_acks.remove(block_number);
            if let Some(instant) = self.send_times.remove(&(ack_header.block_number as usize)) {
                self.update_average_rtt(instant.elapsed());
            }
        }

        use std::cmp::min;
        let new_lower = ack_header.block_number + 1;
        self.window_range = (new_lower, min(new_lower + self.window_size, self.num_blocks));
        
        if self.window_range.0 == self.num_blocks {
            Ok(Async::Ready(()))
        } else {
            self.send_window()?;
            Ok(Async::NotReady)
        }
    }

    fn send_window(&mut self) -> Result<(), io::Error> {
	for block_number in self.window_range.0..self.window_range.1 {
	    if let Some(block) = self.get_block_n(block_number) {
                self.send_data(block)?;
            }
        }
        Ok(())
    }

    fn handle_error(&mut self, err_header: ErrorHeader) -> Poll<(), io::Error> {
        Err(io::Error::new(io::ErrorKind::Other, err_header.error_message))
    }

    fn receive_header(&mut self) -> Result<Option<Header>, io::Error> {
        if let Ok(ref mut socket) = self.socket.clone().try_lock() {
            socket.set_read_timeout(None)?;  
    	    match Header::recv(self.host_addr.clone(), socket) {
                Ok(r)   => { self.err_counter = 0; Ok(Some(r)) },
                Err(e)  => {
                    if self.err_counter > MAX_ATTEMPTS {
                        if let TFTPError::IOError(ioerr) = e {
                            Err(ioerr)
                        } else {
                            Ok(None)
                        }
                    } else {
                        if let TFTPError::IOError(ioerr) = e {
                            match ioerr.kind() { 
                                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => self.send_window()?,
                                _ => {}
                            }
                        }
                        self.err_counter += 1;
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
        if let Ok(ref mut s) = self.socket.try_lock() {
            s.set_read_timeout(Some(self.average_rtt.clone()));
        }
    }
}

impl Future for SendFile {
    type Item = ();
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        if self.window_range.0 == self.num_blocks && self.blocks_pending_acks.is_empty() {
            return Ok(Async::Ready(()));
        } else {
            match self.receive_header() {
                Ok(Some(Header::Ack(ack_header))) => self.handle_ack(ack_header),

                Ok(Some(Header::Error(err_header))) => self.handle_error(err_header),

                // This means either a header type we don't want was received, or a tftp error occured
                // (respectively).
                Ok(Some(_)) | Ok(None) => Ok(Async::NotReady),

                Err(e) => {
                    match e.kind() {
		    	io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => { Ok(Async::NotReady) },
			_ => {
			    eprintln!("Encountered non-recoverable I/O error: {:?}", e);
                	    Err(e)
		    	}
		    }
                },
	    }
        }
    }
}

