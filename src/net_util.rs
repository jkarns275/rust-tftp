use std::net::*;
use local_ip;

lazy_static! {
    pub static ref LOCAL_IP: IpAddr = {
        //match None {
        //    Some(_) => {
        //        println!("Wow!");
        //        panic!("")
        //    },
            /*_ =>*/ IpAddr::V4(Ipv4Addr::new(129, 3, 144, 125))
        //}

    };
}