## Payment Service

This crate is a service to be loaded in Yagna to handle payment scenario's.
The payment service is the main service `yagna` will be talking to, but not directly handling the payments.
The payments are made by drivers loaded in the service.

### Drivers

Currently these drivers are available to use:
- NGNT
- Dummy
- ZK-NGNT

By default the NGNT driver is selected, extra drivers need to be specifically loaded with a feature flag.

## DO NOT USE DUMMY DRIVER FOR BUILDS THAT WILL BE DISTRIBUTED!!!

You can enable multiple drivers at the same time, use this table for the required feature flags and platform parameters:

|Driver name|Platform name|Feature flag|Public explorer|Local|Testnet|Mainnet|
|-|-|-|-|-|-|-|
|zk-ngnt|ZK-NGNT|`ya-zksync-driver`|[zkscan](https://rinkeby.zkscan.io/)|x|x||
|ngnt|NGNT|`ya-ngnt-driver`|[etherscan](https://rinkeby.etherscan.io/token/0xd94e3dc39d4cad1dad634e7eb585a57a19dc7efe)|x|x||
|dummy|DUMMY|`ya-dummy-driver`|None|x|||

### Examples:

Build with only gnt driver:
```
cargo build --release
```

Build with ngnt + zksync
```
cargo build --release --features zksync-driver
```