#![feature(conservative_impl_trait)]

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

#[macro_use]
extern crate lazy_static;

use futures::Future;


pub mod error;
pub mod client;
mod header;
mod net_util;
mod types;

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use futures::*;
    use tokio_core::reactor::Core;


    #[test]
    fn it_works() {
        let mut core = Core::new().unwrap();
        let mut client = client::TFTPClient::new(SocketAddr::from((net_util::LOCAL_IP.clone(), 1921)), 1920).unwrap();
        let mut x = client.send_error(1.into());

        let data = vec![4u8; 512];

        let mut y = client.send_data(&data[..], 0).unwrap();

        core.run(x).unwrap();
        core.run(y).unwrap();
    }
}
