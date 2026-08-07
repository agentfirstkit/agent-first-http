//! `afhttp capabilities` subcommand.

use crate::cli::connect::Connection;
use crate::cli::output;
use crate::shared::error::Error;

#[derive(Debug)]
pub struct Args {
    pub connection: Connection,
}

pub async fn run(args: Args) -> Result<(), Error> {
    let client = args.connection.client().await?;
    let response = client.capabilities().await?;
    output::emit("capabilities", &response)
}
