//! `afhttp health` subcommand.

use clap::Args as ClapArgs;

use crate::cli::output;
use crate::sdk::Client;
use crate::shared::error::Error;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// CDP endpoint of the running host.
    #[arg(long = "endpoint-url")]
    pub endpoint: String,
    /// Bearer token, if the host was started with `--token-secret`.
    #[arg(long = "token-secret")]
    pub token: Option<String>,
}

pub async fn run(args: Args) -> Result<(), Error> {
    let mut client = Client::connect(&args.endpoint)?;
    if let Some(t) = args.token.as_deref() {
        client = client.with_token(t);
    }
    let response = client.health().await?;
    output::emit("health", &response)
}
