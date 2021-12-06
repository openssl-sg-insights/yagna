use std::time::Duration;
use structopt::StructOpt;
use strum::VariantNames;
use strum::{EnumString, EnumVariantNames, IntoStaticStr};

#[derive(StructOpt, EnumString, EnumVariantNames, IntoStaticStr, Clone)]
#[strum(serialize_all = "lowercase")]
pub enum NetType {
    Central,
    Hybrid,
}

#[derive(StructOpt, Clone)]
#[structopt(rename_all = "kebab-case")]
pub struct Config {
    #[structopt(env = "YA_NET_TYPE", possible_values = NetType::VARIANTS, default_value = NetType::Central.into())]
    pub net_type: NetType,
    #[structopt(env = "YA_NET_DEFAULT_PING_INTERVAL", parse(try_from_str = humantime::parse_duration), default_value = "15s")]
    pub ping_interval: Duration,
    #[structopt(env = "YA_NET_RELAY_HOST", default_value = "127.0.0.1:7464")]
    pub host: String,
}

impl Config {
    pub fn from_env() -> Result<Config, structopt::clap::Error> {
        // Empty command line arguments, because we want to use ENV fallback
        // or default values if ENV variables are not set.
        Ok(Config::from_iter_safe(&[""])?)
    }
}

/// TODO: Remove compilation flag.
///  This conditional compilation is hack to make Goth integration tests work.
///  Current solution in Goth is to build separate binary with compilation flag.
///  This is only temporary for transition period, to make this PR as small as possible.
#[cfg(not(feature = "hybrid-net"))]
impl Default for NetType {
    fn default() -> Self {
        NetType::Central
    }
}

#[cfg(feature = "hybrid-net")]
impl Default for NetType {
    fn default() -> Self {
        NetType::Hybrid
    }
}