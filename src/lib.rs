mod pool;

use std::{
    io::{IoSlice, IoSliceMut, Write},
    marker::PhantomData,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    os::unix::prelude::AsRawFd,
    sync::Arc,
    time::Duration,
};

use async_channel::{Receiver, Sender};
use nix::sys::socket::{RecvMmsgData, SockaddrStorage};
use nix::{
    errno::Errno,
    sys::socket::{MsgFlags, SendMmsgData},
};
use pool::BufferPool;

const MAX_SEND_BATCH: usize = 128;
const MAX_RECV_BATCH: usize = 128;

/// A fast, generously-buffered TCP socket backed by a background thread to save on syscalls.
#[derive(Clone, Debug)]
pub struct FastUdpSocket {
    recv_incoming: Receiver<(Vec<u8>, SocketAddr)>,
    send_outgoing: Sender<(Vec<u8>, SocketAddr)>,
    pool: Arc<BufferPool>,
}

impl From<UdpSocket> for FastUdpSocket {
    fn from(s: UdpSocket) -> Self {
        Self::from_std(s)
    }
}

impl FastUdpSocket {
    /// Create a new FastUdpSocket from a standard one
    pub fn from_std(std: UdpSocket) -> Self {
        let (send_incoming, recv_incoming) = async_channel::bounded(MAX_SEND_BATCH * 2);
        let (send_outgoing, recv_outgoing) = async_channel::bounded(MAX_RECV_BATCH * 2);
        let pool = Arc::new(BufferPool::new());
        {
            let pool = pool.clone();
            let std = std.try_clone().expect("cannot clone this socket?!");
            std::thread::Builder::new()
                .name("fastudp-send".into())
                .spawn(move || udp_send_loop(recv_outgoing, std, pool))
                .unwrap();
        }
        {
            let pool = pool.clone();
            let std = std.try_clone().expect("cannot clone this socket?!");
            std::thread::Builder::new()
                .name("fastudp-recv".into())
                .spawn(move || udp_recv_loop(send_incoming, std, pool))
                .unwrap();
        }
        Self {
            recv_incoming,
            send_outgoing,
            pool,
        }
    }

    /// Sends data on the soccket to the given address. On success, returns the number of bytes written.
    pub async fn send_to(&self, buf: &[u8], addr: SocketAddr) -> std::io::Result<usize> {
        let v = self.pool.alloc(buf.len());
        let n = v.len();
        self.send_outgoing
            .send((v, addr))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "broken pipe"))?;
        Ok(n)
    }

    /// Receives data through the socket. On success, returns the number of bytes copied.
    pub async fn recv_from(&self, mut buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
        let (vec, addr) = self
            .recv_incoming
            .recv()
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "broken pipe"))?;
        let n = buf.write(&vec)?;
        self.pool.free(vec);
        Ok((n, addr))
    }
}

fn udp_recv_loop(
    send_incoming: Sender<(Vec<u8>, SocketAddr)>,
    socket: UdpSocket,
    pool: Arc<BufferPool>,
) -> Option<()> {
    socket.set_read_timeout(Some(Duration::from_secs(1)));
    let fd = socket.as_raw_fd();
    let mut buffs = Vec::with_capacity(MAX_RECV_BATCH);
    loop {
        while buffs.len() < MAX_RECV_BATCH {
            buffs.push(pool.alloc(2048));
        }
        // use the buffs to read
        let result = {
            let mut smmsg_buf = buffs
                .iter_mut()
                .map(|buff| RecvMmsgData {
                    iov: [IoSliceMut::new(buff)],
                    cmsg_buffer: None,
                })
                .collect::<Vec<_>>();
            let to_iterate = match nix::sys::socket::recvmmsg::<_, SockaddrStorage>(
                fd,
                smmsg_buf.iter_mut(),
                MsgFlags::empty(),
                None,
            ) {
                Ok(to_iterate) => to_iterate,
                Err(e) => {
                    if e == Errno::EAGAIN {
                        continue;
                    } else {
                        return None;
                    }
                }
            };
            to_iterate
                .into_iter()
                .map(|res| (res.bytes, res.address))
                .collect::<Vec<_>>()
        };
        for ((n, addr), mut buff) in result.into_iter().zip(buffs.drain(..)) {
            if let Some(addr) = addr.and_then(sockaddr_to_socketaddr) {
                buff.truncate(n);
                send_incoming.send_blocking((buff, addr)).ok()?
            }
        }
    }
}

#[allow(clippy::manual_map)]
fn sockaddr_to_socketaddr(s: SockaddrStorage) -> Option<SocketAddr> {
    if let Some(v4) = s.as_sockaddr_in() {
        Some(SocketAddr::new(Ipv4Addr::from(v4.ip()).into(), v4.port()))
    } else if let Some(v6) = s.as_sockaddr_in6() {
        Some(SocketAddr::new(v6.ip().into(), v6.port()))
    } else {
        None
    }
}

fn udp_send_loop(
    recv_outgoing: Receiver<(Vec<u8>, SocketAddr)>,
    socket: UdpSocket,
    pool: Arc<BufferPool>,
) -> Option<()> {
    let mut pkt_buff: Vec<(Vec<u8>, SocketAddr)> = vec![];
    let fd = socket.as_raw_fd();
    loop {
        for buf in pkt_buff.drain(..) {
            pool.free(buf.0);
        }
        pkt_buff.push(recv_outgoing.recv_blocking().ok()?);
        while let Ok(more) = recv_outgoing.try_recv() {
            pkt_buff.push(more);
            if pkt_buff.len() >= MAX_SEND_BATCH {
                break;
            }
        }
        let smmsg_buff = pkt_buff
            .iter()
            .map(|(buf, dest)| SendMmsgData {
                iov: [IoSlice::new(buf)],
                cmsgs: [],
                addr: Some(SockaddrStorage::from(*dest)),
                _lt: PhantomData::default(),
            })
            .collect::<Vec<_>>();
        let res = nix::sys::socket::sendmmsg(fd, smmsg_buff.iter(), MsgFlags::empty()).unwrap();
        log::trace!("batch of {}=>{} sends", pkt_buff.len(), res.len());
    }
}
