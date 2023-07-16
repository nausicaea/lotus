use std::net::{IpAddr, Ipv4Addr};

pub mod assets;
pub mod collectors;
pub mod docker;
pub mod server;
pub mod test_cases;

const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
const INPUT_PORT: u16 = 5066;
const OUTPUT_PORT: u16 = 5067;
const API_PORT: u16 = 9600;
const INPUT_FILE: &'static str = "input.json";
const EXPECTED_FILE: &'static str = "expected.json";
const RULE_EXTENSION: &'static str = "conf";
