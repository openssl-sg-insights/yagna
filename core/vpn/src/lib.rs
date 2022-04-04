pub use self::service::VpnService;

mod device;
mod interface;
mod message;
mod network;
mod port;
mod requestor;
mod service;
mod socket;
mod stack;

pub type Result<T> = std::result::Result<T, ya_utils_networking::vpn::Error>;
