//! `afhttp capabilities` subcommand.

use clap::Args as ClapArgs;

use crate::cli::output;
use crate::sdk::Client;
use crate::shared::error::Error;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// CDP endpoint of the running host (e.g. `ws://127.0.0.1:9222`). Falls back to `AFHTTP_ENDPOINT_URL`.
    #[arg(long = "endpoint-url", env = "AFHTTP_ENDPOINT_URL")]
    pub endpoint: String,
    /// Bearer token, if the host was started with `--token-secret`.
    /// Falls back to `AFHTTP_TOKEN_SECRET`.
    #[arg(long = "token-secret", env = "AFHTTP_TOKEN_SECRET")]
    pub token: Option<String>,
}

pub async fn run(args: Args) -> Result<(), Error> {
    let mut client = Client::connect(&args.endpoint)?;
    if let Some(t) = args.token.as_deref() {
        client = client.with_token(t);
    }
    let response = client.capabilities().await?;
    output::emit("capabilities", &response)
}
