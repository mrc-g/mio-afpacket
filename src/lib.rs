extern crate libc;
extern crate mio;

use mio::event::Source;
use mio::unix::SourceFd;
use mio::{Interest, Registry, Token};
use std::io::{Error, ErrorKind, Read, Result, Write};
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, RawFd};

use libc::{sockaddr_ll, sockaddr_storage, socket, packet_mreq, setsockopt};
use libc::{AF_PACKET, ETH_P_ALL, SOCK_RAW, SOL_PACKET, PACKET_MR_PROMISC,
        PACKET_ADD_MEMBERSHIP, PACKET_DROP_MEMBERSHIP};

/// Packet sockets are used to receive or send raw packets at OSI 2 level.
#[derive(Debug)]
pub struct RawPacketStream(RawFd);

impl Source for RawPacketStream {
    fn register(&mut self, registry: &Registry, token: Token, interest: Interest) -> Result<()> {
        registry.register(&mut SourceFd(&self.0), token, interest)
    }

    fn reregister(&mut self, registry: &Registry, token: Token, interest: Interest) -> Result<()> {
        registry.reregister(&mut SourceFd(&self.0), token, interest)
    }

    fn deregister(&mut self, registry: &Registry) -> Result<()> {
        registry.deregister(&mut SourceFd(&self.0))
    }
}

impl RawPacketStream {
    /// Create new raw packet stream binding to all interfaces
    pub fn new() -> Result<Self> {
        let fd = unsafe { socket(AF_PACKET, SOCK_RAW, i32::from((ETH_P_ALL as u16).to_be())) };
        if fd == -1 {
            return Err(Error::last_os_error());
        }
        let mut nonblocking = 1 as libc::c_ulong;
        let res = unsafe { libc::ioctl(fd, libc::FIONBIO, &mut nonblocking) };

        if res == -1 {
            return Err(Error::last_os_error());
        }
        Ok(RawPacketStream(fd as RawFd))
    }

    /// Bind socket to an interface (by name).
    pub fn bind(&mut self, name: &str) -> Result<()> {
        let idx = index_by_name(name)?;
        self.bind_by_index(idx)
    }

    fn bind_by_index(&mut self, ifindex: i32) -> Result<()> {
        unsafe {
            let mut ss: sockaddr_storage = std::mem::zeroed();
            let sll: *mut sockaddr_ll = &mut ss as *mut sockaddr_storage as *mut sockaddr_ll;
            (*sll).sll_family = AF_PACKET as u16;
            (*sll).sll_protocol = (ETH_P_ALL as u16).to_be();
            (*sll).sll_ifindex = ifindex;

            let sa = (&ss as *const libc::sockaddr_storage) as *const libc::sockaddr;
            let res = libc::bind(self.0, sa, std::mem::size_of::<sockaddr_ll>() as u32);
            if res == -1 {
                return Err(Error::last_os_error());
            }
        }
        Ok(())
    }

    pub fn set_promisc(&mut self, name: &str, state: bool) -> Result<()> {
        let packet_membership = if state {
            PACKET_ADD_MEMBERSHIP
        } else {
            PACKET_DROP_MEMBERSHIP
        };

        let idx = index_by_name(name)?;

        unsafe {
            let mut mreq: packet_mreq = std::mem::zeroed();

            mreq.mr_ifindex = idx;
            mreq.mr_type = PACKET_MR_PROMISC as u16;

            setsockopt(self.0, SOL_PACKET, packet_membership, (&mreq as *const packet_mreq) as *const libc::c_void, std::mem::size_of::<packet_mreq>() as u32);
        }

        Ok(())
    }
}

fn index_by_name(name: &str) -> Result<i32> {
    if name.len() > libc::IFNAMSIZ {
        return Err(ErrorKind::InvalidInput.into());
    }
    let mut buf = [0u8; libc::IFNAMSIZ];
        buf[..name.len()].copy_from_slice(name.as_bytes());
    let idx = unsafe { libc::if_nametoindex(buf.as_ptr() as *const libc::c_char) };
    if idx == 0 {
        return Err(Error::last_os_error());
    }

    Ok(idx as i32)
}

fn read_fd(fd: RawFd, buf: &mut [u8]) -> Result<usize> {
    let rv = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if rv < 0 {
        return Err(Error::last_os_error());
    }

    Ok(rv as usize)
}

impl Read for RawPacketStream {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        read_fd(self.0, buf)
    }
}

impl<'a> Read for &'a RawPacketStream {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        read_fd(self.0, buf)
    }
}

fn write_fd(fd: RawFd, buf: &[u8]) -> Result<usize> {
    let rv = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
    if rv < 0 {
        return Err(Error::last_os_error());
    }

    Ok(rv as usize)
}

impl Write for RawPacketStream {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        write_fd(self.0, buf)
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

impl<'a> Write for &'a RawPacketStream {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        write_fd(self.0, buf)
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

impl IntoRawFd for RawPacketStream {
    fn into_raw_fd(self) -> RawFd {
        self.0
    }
}

impl AsRawFd for RawPacketStream {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl FromRawFd for RawPacketStream {
    unsafe fn from_raw_fd(fd: RawFd) -> RawPacketStream {
        RawPacketStream(fd)
    }
}

// tests require CAP_NET_RAW capabilities
// tests must be executed sequentially
#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use super::RawPacketStream;

    const MESSAGE: &[u8] = b"hello world\n";

    #[test]
    fn it_works() {
        RawPacketStream::new().unwrap();
    }

    #[test]
    fn promisc_mode() {
        let mut rp = RawPacketStream::new().unwrap();
        rp.set_promisc("lo", true).unwrap();
    }

    fn execute_test(test: impl Test) {
        use mio::net::*;
        use mio::*;

        const TCP_SERVER: Token = Token(0);
        const TCP_CLIENT: Token = Token(1);
        const RAW: Token = Token(2);

        const CONNECTION_ID_START: usize = 128;

        let mut id = CONNECTION_ID_START;

        let mut poll = Poll::new().unwrap();
        let mut events = Events::with_capacity(1024);
        let mut connections = HashMap::new();

        let mut tcp_server = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let server_addr = tcp_server.local_addr().unwrap();

        poll.registry().register(&mut tcp_server, TCP_SERVER, Interest::READABLE)
            .unwrap();

        let mut tcp_client = TcpStream::connect(server_addr).unwrap();
        poll.registry().register(&mut tcp_client, TCP_CLIENT, Interest::READABLE)
            .unwrap();

        let mut raw = RawPacketStream::new().unwrap();
        test.setup(&mut raw);
        poll.registry().register(
            &mut raw,
            RAW,
            Interest::READABLE | Interest::WRITABLE,
        ).unwrap();

        loop {
            poll.poll(&mut events, None).unwrap();

            for event in &events {
                match event.token() {
                    TCP_SERVER => {
                        println!("accepting conn");
                        let (mut conn, _) = tcp_server.accept().unwrap();
                        let token = Token(id);
                        poll.registry().register(&mut conn, token, Interest::WRITABLE)
                            .unwrap();
                        connections.insert(token, conn);
                        id += 1;
                    }
                    TCP_CLIENT => {
                        println!("connected");
                    }
                    RAW => {
                        if event.is_readable() {
                            if test.read(&mut raw) {
                                return;
                            }
                        }
                        if event.is_writable() {
                            test.write(&mut raw);
                        }
                    }
                    token => {
                        println!("writing");
                        let mut conn = connections.get(&token).unwrap();
                        conn.write(MESSAGE).unwrap();
                        conn.flush().unwrap();
                    }
                }
            }
        }
    }

    trait Test {
        fn setup(&self, _raw: &mut RawPacketStream) {}
        // return true on success
        fn read(&self, _raw: &mut RawPacketStream) -> bool;
        fn write(&self, _raw: &mut RawPacketStream) {}
    }

    // helper method to extract data from raw packet
    fn parse_packet(bytes: &[u8]) -> Option<&[u8]> {
        // skip mac
        let bytes = &bytes[12..];
        // check ethertype (ip)
        if bytes[0] != 0x8 || bytes[1] != 0 {
            return None;
        }
        let bytes = &bytes[2..];
        // check ip version
        if bytes[0] >> 4 != 4 {
            return None;
        }
        // get ip header length
        let ip_header_len = 4 * (bytes[0] & 0xf) as usize;
        // skip ip header
        let bytes = &bytes[ip_header_len..];
        // get tcp header length
        let tcp_header_len = 4 * (bytes[12] >> 4) as usize;
        let bytes = &bytes[tcp_header_len..];
        Some(bytes)
    }

    fn read_hello_world(raw: &mut RawPacketStream) -> bool {
        let mut buf = [0; 1024];
        if let Ok(len) = raw.read(&mut buf) {
            if let Some(parsed) = parse_packet(&buf[..len]) {
                if parsed == MESSAGE {
                    println!("received msg!");
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn basic_receive() {
        struct Basic;
        impl Test for Basic {
            fn read(&self, raw: &mut RawPacketStream) -> bool {
                read_hello_world(raw)
            }
        }

        impl dyn Test {}
        execute_test(Basic)
    }

    #[test]
    fn bind_and_receive() {
        struct Bind;
        impl Test for Bind {
            fn setup(&self, raw: &mut RawPacketStream) {
                raw.bind("lo").unwrap();
            }
            fn read(&self, raw: &mut RawPacketStream) -> bool {
                read_hello_world(raw)
            }
        }
        execute_test(Bind);
    }

    #[test]
    fn write_and_read() {
        struct ReadWriteEther;
        const SRC_MAC: &[u8] = &[0xd, 0xe, 0xa, 0xd, 0xb, 0xe];

        impl Test for ReadWriteEther {
            fn setup(&self, raw: &mut RawPacketStream) {
                raw.bind("lo").unwrap();
            }
            fn read(&self, raw: &mut RawPacketStream) -> bool {
                let mut buf = [0; 1024];
                if let Ok(len) = raw.read(&mut buf) {
                    println!("pkt: {:02x?}", &buf[..len]);
                    if &buf[6..12] == SRC_MAC {
                        return true;
                    }
                }
                false
            }
            fn write(&self, raw: &mut RawPacketStream) {
                let mut buf = vec![0; 32];
                buf[6..12].clone_from_slice(SRC_MAC);
                raw.write(&buf).unwrap();
            }
        }
        execute_test(ReadWriteEther);
    }
}
