#![feature(string_retain)]
#![feature(test)]

extern crate tftp;
#[macro_use]
extern crate rpds;
extern crate futures;
extern crate hyper;
extern crate tokio_core;
extern crate serde;
extern crate bincode;

use tftp::client::TFTPClient;
use tftp::header::*;
use tftp::send::*;
use tftp::receive::*;
use tftp::error::TFTPError;

use bincode::{ serialize, deserialize };
use futures::{ Future, Stream, Async, Poll };
use hyper::Client;
use tokio_core::reactor::Core;
use rpds::HashTrieMap;
use serde::*;
use tokio_core::reactor::Handle;

use std::time::*;
use std::net::*;
use std::fs::{ self, Metadata, File, OpenOptions };
use std::io::{ self, Write, Read, Seek };
use std::ops::*;


static CLIENT_DOWNLOAD: &'static str = "downloaded/";
static CACHED_FILES_LOCATION: &'static str = "cached_files/";
static CACHE_LOCATION: &'static str = "cache";

/// The cache maps a url to a uniquely named file.
fn get_cache() -> Result<HashTrieMap<String, String>, io::Error> {
    if let Ok(mut file) = OpenOptions::new().read(true).create(false).open(CACHE_LOCATION) {
        let mut buf = vec![];
        file.read_to_end(&mut buf)?;
        let x = deserialize::<HashTrieMap<String, String>>(&buf).unwrap();
        Ok(x)
    } else {
        Ok(HashTrieMap::new())
    }
}

fn add_and_save(cache: HashTrieMap<String, String>, url: String, file: String) -> Result<HashTrieMap<String, String>, io::Error> {
    let new = cache.insert(url, file);
    let mut file = OpenOptions::new().read(true).write(true).truncate(true).create(true).open(CACHE_LOCATION)?;
    file.seek(io::SeekFrom::Start(0))?;
    let dat = serialize(&new).unwrap();
    let a = file.write_all(&dat)?;
    file.flush()?;
    Ok(new)
}

fn get(url: &str, core: &mut Core) -> String {
    let mut cache = get_cache().unwrap();
    if let Some(x) = cache.get(url) {
        return x.to_string();
    }

    let client = Client::new(&core.handle());
    let uri = url.parse().unwrap();
    let work = client.get(uri).map_err(|_err| ()).and_then(|resp| {
        resp.body().concat2().map_err(|_err| ()).map(|chunk| {
            chunk.to_vec()
        })
    });
    let dat = core.run(work).unwrap();
    use std::hash::{ Hasher, Hash, SipHasher };
    let mut hasher = SipHasher::new();
    hasher.write(&dat);
    let filename = 
        hasher
        .finish()
        .to_string()
        .add(&SystemTime::now()
             .duration_since(UNIX_EPOCH)
             .unwrap_or(Duration::from_secs(0))
             .as_secs().
             to_string());

    let path = CACHED_FILES_LOCATION.to_string().add(&filename);
    let mut file = OpenOptions::new()
                            .write(true)
                            .truncate(true)
                            .create(true)
                            .open(path).unwrap();
    
    file.write_all(&dat).unwrap();
    let _ = add_and_save(cache, url.to_string(), filename.clone());
    filename
}

fn server(addr: SocketAddr, window_size: usize) {
    let mut core = Core::new().unwrap();
    let mut server = TFTPClient::new(addr.clone(), addr, CACHED_FILES_LOCATION.to_string(), window_size).unwrap();
    let mut cache = get_cache().unwrap();
    loop {
        let header_result = if let Ok(ref mut socket) = server.udp_socket.try_lock() {
            socket.set_read_timeout(Some(Duration::from_secs(4)));
            Header::peek(socket)
        } else {
            Err(TFTPError::ConnectionClosed)
        };
        if header_result.is_err() { continue }
        if let Ok(ref mut socket) = server.udp_socket.try_lock() {
            let _ = Header::recv(header_result.as_ref().unwrap().1.clone(), socket);
        }
        let mut buf = [0u8; MAX_DATA_LEN * 4];
        match header_result {
            Ok((Header::Read(mut read_header), src)) => {
                println!("Serving {:?}", src);
                server.host_addr = src;
                let url = read_header.filename.clone();
                read_header.filename = get(&url, &mut core);
                cache = add_and_save(cache, url, read_header.filename.clone()).unwrap();
                server.clone().handle_read_request(read_header);
            },
            _ => {
            }, // Ignore everything else
            Err(e) => {}, // oof
        }
    }   
}

fn request(local_addr: SocketAddr, host_addr: SocketAddr, url: String, window_size: usize) {
    let drop_rate = unsafe { tftp::header::DROP_THRESHOLD };
    unsafe { tftp::header::DROP_THRESHOLD = 0; }
    let mut client = TFTPClient::new(host_addr, local_addr, CLIENT_DOWNLOAD.to_string(), window_size).unwrap();
    let mut dest = url.clone();
    dest.retain(|c| (c.is_alphabetic() && c.is_ascii()) || c == '.');
    let mut req = client.request_file(url, &dest);
    unsafe { tftp::header::DROP_THRESHOLD = drop_rate; } 
    loop {
        match req.poll() {
            Err(e) => { panic!(format!("{:?}", e)) },
            Ok(Async::NotReady) => continue,
            _ => return
        }
    }
}

static HELP: &'static str = r#"
client usage: 
    command [options] [local ip] [server ip] [url]
    server ip is, by default, localhost
to run in server mode:
    command [--ipv4]? -s
               ^ optional

options:
--help:         display this menu
-d [n]          drop n packets of every 128
-w [n]          sets window size to n
"#;

fn main() {
    use std::env::*;
    let mut args = args().collect::<Vec<String>>();
    args.drain(0..1);
    pmain(args);
}

fn pmain(mut args: Vec<String>) {
    let argn = args.len();
    let mut window_size = 16;
    let mut drop_freq = 0;
    if argn > 3 || args.contains(&"-s".to_owned()) {
        let end = if argn > 3 { argn - 3 } else { argn };
        for i in 0..end {
            match args[i].to_string().as_ref() {
                "-s" => {
                    if argn <= i + 1 {
                        println!("No server port specified");
                        println!("{}", HELP);
                        return;
                    }
                    let mut i = i;

                    let use_ipv4 = if &args[i + 1] == "--ipv4" { i += 1; true } else { false };
                    let port = if let Ok(p) = args[i + 1].parse::<u16>() {
                        p
                    } else {
                        println!("'{}' is not a valid port", args[0]);
                        println!("{}", HELP);
                        return;
                    };    
                    if use_ipv4 { 
                        server(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), port)), window_size);
                    } else {
                        server(SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0), port, 0, 0)), window_size);
                    }
                    return;
                },
                "--help"    => { println!("{}", HELP); return; },
                "-w"        => {
                    if args.len() > i + 1 {
                        if let Ok(w) = args[i + 1].parse::<usize>() {
                            if w > 0 {
                                window_size = w;
                                continue;
                            }
                        }
                        println!("Expected an integer > 0 after '-w' flag, got '{}'.", args[i + 1]);
                        println!("{}", HELP);
                        return;
                    } else {
                        println!("Expected an integer > 0 after '-w' flag.");
                        println!("{}", HELP);
                        return;
                    }
                },
                "-d"    => {
                    if args.len() > i + 1 {
                        if let Ok(w) = args[i + 1].parse::<usize>() {
                            if w < 127 {
                                drop_freq = w;      
                                continue;
                            }
                        }
                        println!("Expected a positive integer < 128 after '-d' flag, got '{}'.", args[i + 1]);
                        println!("{}", HELP);
                        return;
                    } else {
                        println!("Expected an integer > 0 after '-d' flag.");
                        println!("{}", HELP);
                        return;
                    }
                },
                _ => { continue; }
            }
        }
        args.drain(0..argn - 3);
    } else if argn < 3 {
        println!("{}", HELP);
        return;
    }

    // command [options] [local port] [server ip] [url]
    
    let local_addr = if let Ok(p) = SocketAddr::from_str(&args[0]) {
        p
    } else {
        println!("'{}' is not a valid local ip", args[0]);
        println!("{}", HELP);
        return;
    };   

    use std::str::FromStr;
    let server_addr = if let Ok(p) = SocketAddr::from_str(&args[1]) {
        p
    } else {
        println!("'{}' is not a valid socket address", args[1]);
        println!("{}", HELP);
        return;
    };

    /*let local_addr = if server_addr.is_ipv4() {
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), port))
    } else {
        SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0), port, 0, 0)) 
    };*/

    let url = args[2].clone();

    unsafe { tftp::header::DROP_THRESHOLD = drop_freq as u64; };
    
    request(local_addr, server_addr, url, window_size);
}
/*
extern crate test;

#[cfg(test)]
mod tests {
    use test::Bencher;
    use super::pmain;
    
    #[bench]
    fn bench_pmain_with_drops_ipv6(bencher: &mut Bencher) {
        bencher.iter(|| 
                     pmain("-d 50 -w 16 4445 [::1]:4444 http://wallfon.com/walls/animals/black-and-white-dog-in-flower-field.jpg"
                           .split_whitespace()
                           .map(str::to_string)
                           .collect::<Vec<String>>()))
    }

    #[bench]
    fn bench_pmain_no_drops_ipv6(bencher: &mut Bencher) {
        bencher.iter(|| 
                     pmain("-d 0 -w 16 4446 [::1]:4443 http://wallfon.com/walls/animals/black-and-white-dog-in-flower-field.jpg"
                           .split_whitespace()
                           .map(str::to_string)
                           .collect::<Vec<String>>()))
    }

    #[bench]
    fn bench_pmain_with_drops_ipv4(bencher: &mut Bencher) {
        bencher.iter(|| 
                     pmain("-d 50 -w 16 5445 127.0.0.1:5444 http://wallfon.com/walls/animals/black-and-white-dog-in-flower-field.jpg"
                           .split_whitespace()
                           .map(str::to_string)
                           .collect::<Vec<String>>()))
    }

    #[bench]
    fn bench_pmain_no_drops_ipv4(bencher: &mut Bencher) {
        bencher.iter(|| 
                     pmain("-d 0 -w 16 5446 127.0.0.1:5443 http://wallfon.com/walls/animals/black-and-white-dog-in-flower-field.jpg"
                           .split_whitespace()
                           .map(str::to_string)
                           .collect::<Vec<String>>()))
    }
}*/
