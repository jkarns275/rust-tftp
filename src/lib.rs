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
    fn it_works() {
        let host_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 2711);
        let client_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12711);

        let mut client =
            TFTPClient::new(client_addr, host_addr).unwrap();
        let mut server = TFTPClient::new(host_addr, client_addr).unwrap();

        let p = spawn(move || { server.serve() });
        let q = spawn(move || {
            let mut r = client.request_file(Path::new("test.md"), Path::new("oof.md"));
            loop {
                println!("oof");
                match r.poll() {
                    Ok(Async::Ready(_)) => return,
                    Err(e) => { panic!(e.to_string()) },
                    Ok(Async::NotReady) => continue,
                }
            }
        });

        p.join();
        q.join();
    }
}
