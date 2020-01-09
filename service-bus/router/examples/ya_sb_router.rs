use futures::{FutureExt, TryFutureExt};
use structopt::StructOpt;

#[derive(StructOpt)]
#[structopt(name = "Router", about = "Service Bus Router")]
struct Options {
    #[structopt(short = "l", default_value = "127.0.0.1:8245")]
    ip_port: String,
}

#[tokio::main]
async fn main() {
    let options = Options::from_args();
    let listen_addr = options.ip_port.parse().expect("Invalid ip:port");

    ya_sb_router::bind_router(listen_addr).await;
}