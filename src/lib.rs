#![feature(conservative_impl_trait)]
#![feature(nll)]

/// TFTP is a simple file transfer protocol, thus the name "Trivial File Transfer Protocol."
///
/// TFTP is specified in RFC1350: http://www.ietf.org/rfc/rfc1350.txt
///
/// This library aims implement all of RFC1350, and to provide simple means to use and extend
/// it.
extern crate memmap;
extern crate futures;
extern crate local_ip;
extern crate tokio_core;
extern crate bit_set;
extern crate bit_vec;
extern crate rayon;
extern crate rand;
//#[macro_use] extern crate lazy_static;



pub mod error;
pub mod client;
mod receive;
mod send;
mod header;
mod types;

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use futures::*;
    use tokio_core::reactor::Core;
    use super::client::*;
    use std::net::*;
    use std::thread::spawn;
    use std::path::*;

    #[test]
    fn test_download() {
        return;
        let host_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 2711);
        let client_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12711);

        let mut client =
            TFTPClient::new(client_addr, host_addr, "data/client_data".to_string()).unwrap();
        let mut server = TFTPClient::new(host_addr, client_addr, "data/server_data".to_string()).unwrap();

        let p = spawn(move || { server.serve() });
        let q = spawn(move || {
            let mut r = client.request_file(Path::new("test.md"), Path::new("oof.md"));
            loop {
                match r.poll() {
                    Ok(Async::Ready(_)) => return,
                    Err(e) => { panic!(e.to_string()) },
                    Ok(Async::NotReady) => continue,
                }
            }
        });

        q.join();
    }

    #[test]
    fn test_upload() {
        let host_addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)), 22711);
        let client_addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)), 32711);

        let mut client =
            TFTPClient::new(client_addr, host_addr, "data/client_data".to_string()).unwrap();
        let mut server = TFTPClient::new(host_addr, client_addr, "data/server_data".to_string()).unwrap();

        let p = spawn(move || { server.serve() });
        let q = spawn(move || {
            let mut r = client.send_file(Path::new("woah.jpeg"));
            loop {
                match r.poll() {
                    Ok(Async::Ready(_)) => return,
                    Err(e) => { eprintln!("{}", e.to_string()); break; },
                    Ok(Async::NotReady) => continue,
                }
            }
        });

        q.join();   
    }
}
