use actix::{
    actors::resolver::{Connect, Resolver},
    prelude::*,
};
use actix_utils::oneshot;
use backoff::{backoff::Backoff, ExponentialBackoff};
use futures::FutureExt;
use redis_async::{
    error::Error as RespError,
    resp::{RespCodec, RespValue},
};
use std::{collections::VecDeque, io};
use tokio::io::{split, WriteHalf};
use tokio::net::TcpStream;
use tokio_util::codec::FramedRead;

use crate::Error;

/// Command for send data to Redis
#[derive(Debug)]
pub struct Command(pub RespValue);

impl Message for Command {
    type Result = Result<RespValue, Error>;
}

/// Redis comminucation actor
pub struct RedisActor {
    addr: String,
    db: usize,
    password: Option<String>,
    backoff: ExponentialBackoff,
    cell: Option<actix::io::FramedWrite<WriteHalf<TcpStream>, RespCodec>>,
    queue: VecDeque<oneshot::Sender<Result<RespValue, Error>>>,
}

impl RedisActor {
    /// Start new `Supervisor` with `RedisActor`.

    pub fn start<S: Into<String>>(
        addr: S,
        db: usize,
        password: Option<String>,
    ) -> Addr<RedisActor> {
        let addr = addr.into();

        let mut backoff = ExponentialBackoff::default();
        backoff.max_elapsed_time = None;

        Supervisor::start(move |_| RedisActor {
            addr,
            db,
            password,
            cell: None,
            backoff,
            queue: VecDeque::new(),
        })
    }
}

impl Actor for RedisActor {
    type Context = Context<Self>;

    fn started(&mut self, ctx: &mut Context<Self>) {
        Resolver::from_registry()
            .send(Connect::host(self.addr.as_str()))
            .into_actor(self)
            .map(|res, act, ctx| match res {
                Ok(res) => {
                    match res {
                        Ok(stream) => {
                            info!("Connected to redis server: {}", act.addr);

                            let (r, w) = split(stream);

                            // configure write side of the connection
                            let mut framed = actix::io::FramedWrite::new(w, RespCodec, ctx);
                            if let Some(password) = &act.password {
                                framed.write(resp_array!["AUTH", password.to_string()]);
                            }
                            framed.write(resp_array!["SELECT", act.db.to_string()]);
                            act.cell = Some(framed);

                            // read side of the connection
                            ctx.add_stream(FramedRead::new(r, RespCodec));

                            act.backoff.reset();
                        }
                        Err(err) => {
                            error!("Can not connect to redis server: {}", err);
                            // re-connect with backoff time.
                            // we stop current context, supervisor will restart it.
                            if let Some(timeout) = act.backoff.next_backoff() {
                                ctx.run_later(timeout, |_, ctx| ctx.stop());
                            }
                        }
                    }
                }
                Err(err) => {
                    error!("Can not connect to redis server: {}", err);
                    // re-connect with backoff time.
                    // we stop current context, supervisor will restart it.
                    if let Some(timeout) = act.backoff.next_backoff() {
                        ctx.run_later(timeout, |_, ctx| ctx.stop());
                    }
                }
            })
            .wait(ctx);
    }
}

impl Supervised for RedisActor {
    fn restarting(&mut self, _: &mut Self::Context) {
        self.cell.take();
        for tx in self.queue.drain(..) {
            let _ = tx.send(Err(Error::Disconnected));
        }
    }
}

impl actix::io::WriteHandler<io::Error> for RedisActor {
    fn error(&mut self, err: io::Error, _: &mut Self::Context) -> Running {
        warn!("Redis connection dropped: {} error: {}", self.addr, err);
        Running::Stop
    }
}

impl StreamHandler<Result<RespValue, RespError>> for RedisActor {
    fn handle(&mut self, msg: Result<RespValue, RespError>, _: &mut Self::Context) {
        match msg {
            Err(e) => {
                if let Some(tx) = self.queue.pop_front() {
                    let _ = tx.send(Err(e.into()));
                }
            }
            Ok(val) => {
                if let Some(tx) = self.queue.pop_front() {
                    let _ = tx.send(Ok(val));
                }
            }
        }
    }
}

impl Handler<Command> for RedisActor {
    type Result = ResponseFuture<Result<RespValue, Error>>;

    fn handle(&mut self, msg: Command, _: &mut Self::Context) -> Self::Result {
        let (tx, rx) = oneshot::channel();
        if let Some(ref mut cell) = self.cell {
            self.queue.push_back(tx);
            cell.write(msg.0);
        } else {
            let _ = tx.send(Err(Error::NotConnected));
        }

        Box::pin(rx.map(|res| match res {
            Ok(res) => res,
            Err(_) => Err(Error::Disconnected),
        }))
    }
}
