use std::net::SocketAddr;
use bit_set::BitSet;
use bit_vec::BitVec;
use std::fs::File;
use std::io::{ self, Seek, Read, Write };
use std::path::Path;
use futures::{ Future, Poll, Async };
use std::net::UdpSocket;
use std::time::Duration;
use std::sync::{ Arc, Mutex };
use memmap::{ MmapOptions, MmapMut };
use std::time::Instant;
use std::collections::{ BinaryHeap, HashMap };
use error::TFTPError;
use std::ops::*;

use types::*;
use header::*;
use client::*;

pub struct ReceiveFile {
    /// The file that backs file_map.
    file: File,

    file_map: MmapMut,

    /// The highest block number that has been received. If this is surpassed, then [file_map] must
    /// be increased in size. If it is `None` that means no blocks have been received yet.
    highest_block: Option<usize>,

    received_last_block: bool,

    /// A set that contains the block_number of received blocks.
    received: BitSet,

    socket: Arc<Mutex<UdpSocket>>,

    host_addr: SocketAddr,

    /// The number of errors that have occured sequentially (i.e. one after the other)
    error_count: usize,

    /// The time at which the last data packet was received.
    last_time: Instant
}

impl ReceiveFile {
    pub fn receive(socket: Arc<Mutex<UdpSocket>>, host_addr: SocketAddr, file: File) -> Result<Self, io::Error> {
        let mut r = ReceiveFile::new(socket, host_addr, file)?;
        r.init()
    }

    pub fn new(socket: Arc<Mutex<UdpSocket>>, host_addr: SocketAddr, mut file: File) -> Result<Self, io::Error> {
        println!("OOOOOF");
        file.write(&[65])?;
        let file_map = unsafe { MmapOptions::new().map_mut(&file)? };
        println!("AAAA");
        let mut r = ReceiveFile {
            file,
            file_map,
            socket,
            host_addr,
            received: BitSet::new(),
            received_last_block: false,
            highest_block: None,
            error_count: 0,
            last_time: Instant::now()
        };
        r.init()
    }

    fn init(mut self) -> Result<Self, io::Error> {
        self.send_ack(0)?;
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

    pub fn handle_data(&mut self, data: DataHeader) -> Result<Option<()>, io::Error> {
        self.last_time = Instant::now();

        if let Some(highest_block) = self.highest_block.take() {
            if highest_block < data.block_number {
                self.highest_block = Some(data.block_number);
                self.file_map.flush()?;
                let current_len = self.file_map.len();
                self.file.set_len((MAX_DATA_LEN * (data.block_number as usize - 1) + data.data_len) as u64)?;
            }
        } else {
            self.highest_block = Some(0);
            self.file_map.flush()?;
            let current_len = self.file_map.len();
            self.file.set_len(data.data_len as u64)?;
        }

        self.received.insert(data.block_number as usize);

        println!("HMM");
        // This means it is the last data header.
        if data.data_len < MAX_DATA_LEN {
            self.received_last_block = true;
            println!("SAD");
            self.file_map[data.block_number * MAX_DATA_LEN..]
                .copy_from_slice(&data.data[0..data.data_len]);
            println!("AA");
            self.send_ack(data.block_number)
        } else {
            self.file_map[data.block_number * MAX_DATA_LEN..data.block_number * MAX_DATA_LEN + MAX_DATA_LEN]
                .copy_from_slice(&data.data);
            self.send_ack(data.block_number as usize)
        }
    }

    /// # Returns
    /// Ok(None): if the socket can't be borrowed (it is already being used)
    ///
    /// Ok(Some(())): if the ack was successfully sent
    ///
    /// Err(<io::Error>): If there was an I/O error at any point.
    fn send_ack(&mut self, block_number: usize) -> Result<Option<()>, io::Error> {
        if let Ok(ref mut socket) = self.socket.try_lock() {
            Header::Ack(AckHeader::new(block_number))
                .send(self.host_addr.clone(), socket)?;
            Ok(Some(()))
        } else {
            Ok(None)
        }
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

    fn fail(&mut self, err: io::Error) -> Poll<(), io::Error> {
        for i in 0..MAX_ATTEMPTS {
            if let Ok(ref mut socket) = self.socket.try_lock() {
                match Header::Error(ErrorHeader { error_code: 0u16.into(), error_message: "Giving up 😞".to_string() })
                    .send(self.host_addr.clone(), socket) {
                    Err(e) => continue,
                    _ => return Err(err)
                }
            }
        }
        Err(err)
    }
}

impl Future for ReceiveFile {
    type Item = ();
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        use header::Header::*;

        if self.received_last_block {
            let mut contains_all = true;
            for i in (0..self.highest_block.unwrap()).rev() {
                contains_all &= self.received.contains(i);
                if !contains_all { break }
            }
            if contains_all {
                return Ok(Async::Ready(()))
            }
        }

        if self.last_time.elapsed() > TOTAL_TIMEOUT() {
            return self.fail(io::Error::new(io::ErrorKind::TimedOut, "TFTP connection appears to be dead."))
        }

        let prev_error_count = self.error_count;
        self.error_count = 0;
        match self.receive_header() {
            Ok(Some(Data(data_header))) => {
                // If writing to the file fails, try several times. If it continues to fail, give
                // up.
                println!("Angery");
                for i in 0..MAX_ATTEMPTS {
                    match self.handle_data(data_header.clone()) {
                        Err(e) => {
                            if i == MAX_ATTEMPTS - 1 {
                                return self.fail(e)
                            } else {
                                continue
                            }
                        },
                        // The lock could not be acquired :(
                        Ok(None) => {
                            if i == MAX_ATTEMPTS - 1 {
                                return self.fail(io::Error::new(io::ErrorKind::WouldBlock, "Could not obtain UdpSocket mutex."))
                            } else {
                                continue
                            }
                        },
                        // We did it!
                        Ok(Some(())) => {
                            return Ok(Async::NotReady)
                        }
                    }
                }
                unreachable!()
            },

            Ok(Some(Error(error_header))) =>
                return Err(io::Error::new(io::ErrorKind::Other,
                                          format!("Received error from server: '{}'", error_header.error_message))),

            Ok(Some(_)) | Ok(None) => return Ok(Async::NotReady),

            Err(e) => {
                self.error_count = prev_error_count + 1;
                if self.error_count > MAX_ATTEMPTS {
                    return self.fail(e)
                } else {
                    return Ok(Async::NotReady)
                }
            }
        }
    }
}