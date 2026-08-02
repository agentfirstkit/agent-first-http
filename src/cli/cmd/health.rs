//! `afhttp health` subcommand.

use crate::cli::output;
use crate::sdk::Client;
use crate::shared::error::Error;

#[derive(Debug)]
pub struct Args {
    pub endpoint: String,
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
