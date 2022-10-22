use std::{
    net::{SocketAddr, UdpSocket},
    time::{Duration, Instant},
};

use argh::FromArgs;
use fastudp::FastUdpSocket;

#[derive(FromArgs)]
/// Starts a new UDP spammer
struct Args {
    /// what to listen on and send spam from
    #[argh(option, short = 'l')]
    listen: SocketAddr,

    /// who to spam
    #[argh(option, short = 'c')]
    connect: SocketAddr,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    smolscale::block_on(async {
        let args: Args = argh::from_env();
        let usock = FastUdpSocket::from(UdpSocket::bind(args.listen)?);
        // let usock = smol::net::UdpSocket::bind(args.listen).await?;
        {
            let usock = usock.clone();
            smolscale::spawn::<anyhow::Result<()>>(async move {
                let mut buff = [0u8; 4096];
                loop {
                    let start = Instant::now();
                    for _ in 0..100_000 {
                        let a = usock.recv_from(&mut buff).await?;
                        eprintln!("recv {:?}", a);
                    }
                    let elapsed = start.elapsed();
                    eprintln!(
                        "recv 100 Kpkts in {:.2}s ({:.2} Mpps)",
                        elapsed.as_secs_f64(),
                        0.1 / elapsed.as_secs_f64()
                    )
                }
            })
            .detach()
        }
        loop {
            let spam_msg = [0u8; 1024];
            let start = Instant::now();
            for _ in 0..100_000 {
                usock.send_to(&spam_msg, args.connect).await?;
                eprintln!("send");
                smol::Timer::after(Duration::from_secs(1)).await;
            }
            let elapsed = start.elapsed();
            eprintln!(
                "send 100 Kpkts in {:.2}s ({:.2} Mpps)",
                elapsed.as_secs_f64(),
                0.1 / elapsed.as_secs_f64()
            )
        }
    })
}
