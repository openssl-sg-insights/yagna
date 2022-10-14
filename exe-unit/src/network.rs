use std::convert::TryFrom;
use std::path::Path;

use bytes::{Buf, BytesMut};
use futures::Stream;
use tokio::sync::mpsc;

use ya_runtime_api::deploy::ContainerEndpoint;
use ya_runtime_api::server::Network;
use ya_service_bus::{typed, typed::Endpoint as GsbEndpoint};
use ya_utils_networking::vpn::common::DEFAULT_MAX_FRAME_SIZE;
use ya_utils_networking::vpn::{network::DuoEndpoint, Error as NetError};

use crate::error::Error;
use crate::state::DeploymentNetwork;
use crate::Result;

pub(crate) mod inet;
pub(crate) mod vpn;

pub(crate) struct Endpoint {
    tx: mpsc::UnboundedSender<Result<Vec<u8>>>,
    rx: Option<Box<dyn Stream<Item = Result<Vec<u8>>> + Unpin>>,
}

impl Endpoint {
    pub async fn connect(endpoint: impl Into<ContainerEndpoint>) -> Result<Self> {
        match endpoint.into() {
            ContainerEndpoint::Socket(path) => Self::connect_to_socket(path).await,
            ep => Err(Error::Other(format!("Unsupported endpoint type: {:?}", ep))),
        }
    }

    #[cfg(unix)]
    async fn connect_to_socket<P: AsRef<Path>>(path: P) -> Result<Self> {
        use futures::StreamExt;
        use tokio::io::{self, AsyncReadExt, AsyncWriteExt};
        use tokio_stream::wrappers::UnboundedReceiverStream;

        const BUFFER_SIZE: usize = (DEFAULT_MAX_FRAME_SIZE + 2) * 4;

        type SocketChannel = (
            mpsc::UnboundedSender<Result<Vec<u8>>>,
            mpsc::UnboundedReceiver<Result<Vec<u8>>>,
        );

        let socket = tokio::net::UnixStream::connect(path.as_ref()).await?;
        let (read, mut write) = io::split(socket);
        let (tx_si, rx_si): SocketChannel = mpsc::unbounded_channel();

        let stream = {
            let buffer: [u8; BUFFER_SIZE] = [0u8; BUFFER_SIZE];
            futures::stream::unfold((read, buffer), |(mut r, mut b)| async move {
                match r.read(&mut b).await {
                    Ok(0) => None,
                    Ok(n) => Some((Ok(b[..n].to_vec()), (r, b))),
                    Err(e) => Some((Err(e.into()), (r, b))),
                }
            })
            .boxed_local()
        };

        tokio::task::spawn(async move {
            let mut rx_si = UnboundedReceiverStream::new(rx_si);
            loop {
                match StreamExt::next(&mut rx_si).await {
                    Some(Ok(data)) => {
                        if let Err(e) = write.write_all(data.as_slice()).await {
                            log::error!("error writing to VM socket endpoint: {e}");
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        log::error!("VM socket endpoint error: {e}");
                        break;
                    }
                    None => break,
                }
            }
        });

        Ok(Self {
            tx: tx_si,
            rx: Some(Box::new(stream)),
        })
    }

    #[cfg(not(unix))]
    async fn connect_to_socket<P: AsRef<Path>>(_path: P) -> Result<Self> {
        Err(Error::Other("OS not supported".into()))
    }
}

impl<'a> TryFrom<&'a DeploymentNetwork> for Network {
    type Error = Error;

    fn try_from(net: &'a DeploymentNetwork) -> Result<Self> {
        let ip = net.network.addr();
        let mask = net.network.netmask();
        let gateway = net
            .network
            .hosts()
            .find(|ip_| ip_ != &ip)
            .ok_or(NetError::NetAddrTaken(ip))?;

        Ok(Network {
            addr: ip.to_string(),
            gateway: gateway.to_string(),
            mask: mask.to_string(),
            if_addr: net.node_ip.to_string(),
        })
    }
}

type Prefix = u16;
const PREFIX_SIZE: usize = std::mem::size_of::<Prefix>();

pub struct RxBuffer {
    inner: BytesMut,
}

impl Default for RxBuffer {
    fn default() -> Self {
        Self {
            inner: BytesMut::with_capacity(2 * (PREFIX_SIZE + DEFAULT_MAX_FRAME_SIZE)),
        }
    }
}

impl RxBuffer {
    pub fn process(&mut self, received: Vec<u8>) -> RxIterator {
        self.inner.extend(received);
        RxIterator { buffer: self }
    }
}

pub struct RxIterator<'a> {
    buffer: &'a mut RxBuffer,
}

impl<'a> Iterator for RxIterator<'a> {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(len) = read_prefix(&self.buffer.inner) {
            return take_next(&mut self.buffer.inner, len);
        }
        None
    }
}

fn take_next(src: &mut BytesMut, len: Prefix) -> Option<Vec<u8>> {
    let len = len as usize;
    let p_len = PREFIX_SIZE + len;
    if src.len() >= p_len {
        src.advance(PREFIX_SIZE);
        return Some(src.split_to(len).to_vec());
    }
    None
}

fn read_prefix(src: &[u8]) -> Option<Prefix> {
    if src.len() < PREFIX_SIZE {
        return None;
    }
    let mut u16_buf = [0u8; PREFIX_SIZE];
    u16_buf.copy_from_slice(&src[..PREFIX_SIZE]);
    Some(u16::from_ne_bytes(u16_buf))
}

fn write_prefix(dst: &mut Vec<u8>) {
    let len_u16 = dst.len() as u16;
    dst.reserve(PREFIX_SIZE);
    dst.splice(0..0, u16::to_ne_bytes(len_u16).to_vec());
}

fn gsb_endpoint(node_id: &str, net_id: &str) -> DuoEndpoint<GsbEndpoint> {
    DuoEndpoint {
        tcp: typed::service(format!("/net/{}/vpn/{}", node_id, net_id)),
        udp: typed::service(format!("/udp/net/{}/vpn/{}/raw", node_id, net_id)),
    }
}

#[cfg(test)]
mod test {
    use std::iter::FromIterator;

    use super::{write_prefix, RxBuffer};

    enum TxMode {
        Full,
        Chunked(usize),
    }

    impl TxMode {
        fn split(&self, v: Vec<u8>) -> Vec<Vec<u8>> {
            match self {
                Self::Full => vec![v],
                Self::Chunked(s) => v[..].chunks(*s).map(|c| c.to_vec()).collect(),
            }
        }
    }

    #[test]
    fn rx_buffer() {
        for tx in vec![TxMode::Full, TxMode::Chunked(1), TxMode::Chunked(2)] {
            for sz in [1, 2, 3, 5, 7, 12, 64] {
                let src = (0..=255u8)
                    .into_iter()
                    .map(|e| Vec::from_iter(std::iter::repeat(e).take(sz)))
                    .collect::<Vec<_>>();

                let mut buf = RxBuffer::default();
                let mut dst = Vec::with_capacity(src.len());

                src.iter().cloned().for_each(|mut v| {
                    write_prefix(&mut v);
                    for received in tx.split(v) {
                        for item in buf.process(received) {
                            dst.push(item);
                        }
                    }
                });

                assert_eq!(src, dst);
            }
        }
    }
}
