#[macro_use]
extern crate log;
#[macro_use]
extern crate redis_async;
#[macro_use]
extern crate derive_more;
mod redis;
pub use redis::{Command, RedisActor};

/// General purpose actix redis error
#[derive(Debug, Display, From)]
pub enum Error {
    #[display(fmt = "Redis error {}", _0)]
    Redis(redis_async::error::Error),
    /// Receiving message during reconnecting
    #[display(fmt = "Redis: Not connected")]
    NotConnected,
    /// Cancel all waters when connection get dropped
    #[display(fmt = "Redis: Disconnected")]
    Disconnected,
}

// re-export
pub use redis_async::{error::Error as RespError, resp::RespValue};
