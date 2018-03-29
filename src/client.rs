use std::net::SocketAddr;
use std::fs::*;
use std::io;
use futures::{ Future, Poll, Async };
use std::net::UdpSocket;
use std::time::Duration;
use std::sync::{ Arc, Mutex };
use error::TFTPError;
use std::ops::*;
use std::str::FromStr;
use std::path::Path;
use futures::prelude::*;
use futures::future;

use types::*;
use header::*;
use send::*;
use receive::ReceiveFile;

pub const MAX_ATTEMPTS: usize = 8;

#[derive(Clone)]
pub struct TFTPClient {
    pub host_addr: SocketAddr,
    data_folder: String,
    pub window_size: usize,
    pub udp_socket: Arc<Mutex<UdpSocket>>
}

unsafe impl Send for TFTPClient {}
unsafe impl Sync for TFTPClient {}

impl TFTPClient {
    pub fn new(host_addr: SocketAddr, socket_addr: SocketAddr, data_folder: String, window_size: usize) -> Result<Self, io::Error> {
        let mut udp_socket: UdpSocket = UdpSocket::bind(socket_addr)?;
        udp_socket.set_read_timeout(Some(Duration::from_secs(4)))?;
        udp_socket.set_write_timeout(Some(Duration::from_secs(4)))?;

        Ok(TFTPClient {
            window_size,
            data_folder,
            host_addr,
            udp_socket: Arc::new(Mutex::new(udp_socket))
        })
    }

    //fn connect_to_host(host_addr: SocketAddr) -> impl Future<Item=(), Error=io::Error> { unimplemented!() }
    //pub fn send_file<P: AsRef<Path>, S: AsRef<Path>>(source: P, filename: S) -> impl Future<Item=i32, Error=io::Error> { unimplemented!() }

    pub fn request_file<P: AsRef<Path>, S: AsRef<Path>>(&mut self, filename: P, destination: S) -> impl Future<Item=(), Error=io::Error> {
        let dest_path: &Path = destination.as_ref();
        let dest = self.data_folder.clone().add("/").add(dest_path.to_str().unwrap());
        let filename = filename.as_ref().to_str().unwrap().to_string();

        let addr = self.host_addr.clone();
        let mut socket = self.udp_socket.clone();
        let read_header = Header::Read(RWHeader::<ReadHeader>::new(filename, RWMode::Octet).unwrap());
        let send_read = future::ok::<u32, u32>(1).then(move |_| {
            let r = if let Ok(ref mut sock) = socket.try_lock() {
                match read_header.send(addr, sock) {
                    Ok(_) => Ok(Async::Ready(())),
                    Err(e) => Err(e)
                }
            } else {
                Err(io::Error::new(io::ErrorKind::Other, "Failed to obtain UDP Socket lock."))
            };
            r
        });

        let addr = self.host_addr.clone();
        let socket = self.udp_socket.clone();
        send_read.and_then(move |_| {
            let mut run =
                ReceiveFile::new(socket, addr,
                                 OpenOptions::new()
                                     .read(true)
                                     .write(true)
                                     .create(true)
                                     .open(dest)?)?;
                run.run()
        })
    }

    pub fn send_file<P: AsRef<Path>>(&mut self, filename: P) -> impl Future<Item=(), Error=io::Error> {
        let filename = filename.as_ref().to_str().unwrap().to_string();
        let file_src = self.data_folder.clone().add("/").add(&filename);
        let addr = self.host_addr.clone();
        let mut socket = self.udp_socket.clone();
        let write_header = Header::Write(RWHeader::<WriteHeader>::new(filename, RWMode::Octet).unwrap());
        let send_read = future::ok::<u32, u32>(1).then(move |_| {
            let r = if let Ok(ref mut sock) = socket.try_lock() {
                match write_header.send(addr, sock) {
                    Ok(_) => Ok(Async::Ready(())),
                    Err(e) => Err(e)
                }
            } else {
                Err(io::Error::new(io::ErrorKind::Other, "Failed to obtain UDP Socket lock."))
            };
            r
        });

        let window_size = self.window_size;
        let addr = self.host_addr.clone();
        let socket = self.udp_socket.clone();
        send_read.and_then(move |_| {
            let mut run =
                SendFile::new(socket, addr,
                                 OpenOptions::new()
                                     .read(true)
                                     .write(false)
                                     .create(false)
                                     .open(file_src)?, window_size)?;
                run.run()
        })
    }
    
    pub fn send_error(&mut self, error: ErrorCode) -> impl Future<Item=(), Error=io::Error> {
        SendError::new(ErrorHeader::new(error, "<No description supplied>".to_string()).unwrap(), self.host_addr.clone(), self.udp_socket.clone())
    }

    pub fn receive_header(&mut self) -> Result<Option<Header>, io::Error> {
        if let Ok(ref mut socket) = self.udp_socket.try_lock() {
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

    pub fn handle_write_request(&mut self, write_header: RWHeader<WriteHeader>) -> Result<(), io::Error> {
        let path = self.data_folder.clone().add("/").add(&write_header.filename);
        let mut file = OpenOptions::new().truncate(true).create(true).read(true).write(true).open(path)?;
        let mut recv_file = ReceiveFile::new(self.udp_socket.clone(), self.host_addr.clone(), file)?;
        recv_file.run()
    }

    pub fn handle_read_request(&mut self, read_header: RWHeader<ReadHeader>) -> Result<(), io::Error> {
        let mut file = match File::open(self.data_folder.clone().add("/").add(&read_header.filename)) {
            Ok(a) => a,
            Err(e) => {
                let mut send_err = self.send_error(ErrorCode::FileNotFound);
                loop {
                    match send_err.poll() {
                        Ok(Async::Ready(_)) => return Err(e),
                        Ok(Async::NotReady) => continue,
                        Err(e) => panic!(format!("{}", e)),
                    }
                }
            }
        };
        let mut send_file = SendFile::new_server(self.udp_socket.clone(), self.host_addr.clone(), file, self.window_size).unwrap();
        send_file.run()
    }

    pub fn handle_server_request(mut self, src: SocketAddr) {
        if let Ok(Some(header)) = self.receive_header() {
            match header {
                Header::Write(write_header) => {
                    self.handle_write_request(write_header);
                },
                Header::Read(read_header) => {
                    self.handle_read_request(read_header);
                },
                _ => return
            }
        }
    }

    pub fn serve(mut self) {
        use rayon::*;
        use std::thread;

        let mut pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();
        let self_copy = self.clone();

        loop {
            let header_result = if let Ok(ref mut socket) = self.udp_socket.try_lock() {
                Header::peek(socket)
            } else {
                Err(TFTPError::ConnectionClosed)
            };
            let mut buf = [0u8; MAX_DATA_LEN * 4];
            match header_result {
                Ok((Header::Read(read_header), src)) => {
                    let mut outgoing_self_copy = self_copy.clone();
                    outgoing_self_copy.host_addr = src;
                    pool.install(move || { outgoing_self_copy.handle_server_request(src) });
                },
                Ok((Header::Write(write_header), src)) => {
                    let mut outgoing_self_copy = self_copy.clone();
                    outgoing_self_copy.host_addr = src;
                    pool.install(move || { outgoing_self_copy.handle_server_request(src) });
                },
                _ => {
                }, // Ignore everything else
                Err(e) => {}, // oof
            }
            // Wait for a read or write request
            // when that is received, move to a new thread and:
                // send an ack to ithe request
                // call send_file / receive file accordingly
            thread::sleep_ms(100);
        }
    }

    /*
    pub fn send_data(&mut self, data: &[u8], block_number: u32) -> Option<impl Future<Item=u32, Error=io::Error>> {
        SendData::new(data, block_number, self.host_addr.clone(), self.udp_socket.clone())
    }
    */
}


/// `TOTAL_TIMEOUT` is the amount of time that, after having not received anything, will mean the
/// whole file-transfer process will have timed out
#[allow(non_snake_case)]
pub fn TOTAL_TIMEOUT() -> Duration { Duration::from_secs(10) }




use std::cmp::*;


pub struct SendData {
    /// The encoded header
    raw_header: RawRequest,

    pub send_attempts: usize,

    /// UDP Socket handle
    socket: Arc<Mutex<UdpSocket>>,

    host_addr: SocketAddr,

    pub block_number: usize
}

impl SendData {
    pub fn new(data: &[u8], block_number: usize, host_addr: SocketAddr, socket: Arc<Mutex<UdpSocket>>) -> Option<SendData> {
        let data_header = DataHeader::new(data, block_number);
        Some(SendData { raw_header: data_header.into(), send_attempts: 0, block_number, socket, host_addr })
    }

    pub fn new_empty(block_number: usize, host_addr: SocketAddr, socket: Arc<Mutex<UdpSocket>>) -> SendData {
        SendData {
            raw_header: DataHeader::new_empty(block_number).into(),
            send_attempts: 0,
            host_addr,
            socket,
            block_number
        }
    }
}

impl Future for SendData {
    type Item = usize;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        if let Ok(ref mut socket) = self.socket.try_lock() {
            match socket.send_to(self.raw_header.as_ref(), self.host_addr) {
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
    pub host_addr: SocketAddr,
    socket: Arc<Mutex<UdpSocket>>,
    pub send_attempts: usize,
    pub raw_header: RawRequest
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
