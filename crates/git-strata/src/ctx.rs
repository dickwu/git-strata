//! What every command needs: the paths, the configuration, the pins and
//! the RPC client.

use serde_json::Value;
use strata_types::Config;

use crate::Result;
use crate::env::Paths;
use crate::rpc::{self, CallError, Kind};
use crate::tty::Pins;

pub struct Ctx {
    pub paths: Paths,
}

impl Ctx {
    pub fn from_env() -> Result<Ctx> {
        Ok(Ctx { paths: Paths::from_env()? })
    }

    pub fn config(&self) -> Result<Config> {
        self.paths.config()
    }

    pub fn pins(&self) -> Pins {
        Pins::load(&self.paths.pins)
    }

    /// A call whose "daemon not listening" is `Busy daemon_down`.
    pub fn call(&self, method: &str, params: Value, kind: Kind) -> Result<Value> {
        rpc::call(&self.paths.socket, method, params, kind).map_err(CallError::into_error)
    }

    /// rm-followups R2: a call that a SIGINT/SIGTERM stops (see
    /// `rpc::call_interruptible`).
    pub fn call_interruptible(
        &self,
        method: &str,
        params: Value,
        kind: Kind,
    ) -> std::result::Result<Result<Value>, rpc::Stopped> {
        rpc::call_interruptible(&self.paths.socket, method, params, kind, &crate::interrupt::STOP)
            .map(|r| r.map_err(CallError::into_error))
    }

    /// A call that tells "nothing listening" apart (reads fall back).
    pub fn try_call(&self, method: &str, params: Value, kind: Kind) -> std::result::Result<Value, CallError> {
        rpc::call(&self.paths.socket, method, params, kind)
    }
}
