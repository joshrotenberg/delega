//! Running the request on a Tokio runtime until its one result.

use std::time::Duration;

use delega_core::contract::request::Request;
use delega_core::contract::result::RunResult;
use delega_core::provider::{ProviderOptions, start};
use tokio::runtime::Builder;

use super::output::refused_result;
use super::request::Facts;
use super::signals::{Signals, follow};

/// How long dropping the runtime waits for blocking helper threads. The run
/// has already ended by then, so this only bounds a stuck helper.
const RUNTIME_SHUTDOWN_WAIT: Duration = Duration::from_secs(5);

/// Run `request` and return its result. A failure to set up the runtime or the
/// signal handlers is a refused result: nothing was started, and starting a
/// worker `delega` could not stop would be worse.
pub fn run(request: Request, options: ProviderOptions, facts: &Facts) -> RunResult {
    let runtime = match Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(err) => {
            return refused_result(
                facts,
                "runtime_unavailable",
                &format!("cannot start the async runtime: {err}"),
            );
        }
    };
    let result = runtime.block_on(async {
        match Signals::install() {
            Ok(signals) => follow(start(request, options), signals).await,
            Err(err) => refused_result(
                facts,
                "signals_unavailable",
                &format!("cannot install the signal handlers that stop the worker: {err}"),
            ),
        }
    });
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_WAIT);
    result
}
