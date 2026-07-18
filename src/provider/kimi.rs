pub mod auth;
pub mod client;
pub mod request;
pub mod stream;

pub const WIRE_MODEL: &str = "kimi-for-coding";

pub fn is_model(id: &str) -> bool {
    matches!(id, WIRE_MODEL | "kimi-k2.6" | "k2.6")
}
