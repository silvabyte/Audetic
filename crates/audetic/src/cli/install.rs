use anyhow::Result;

use super::args::InstallCliArgs;
use crate::install::{run as run_install, InstallOptions};

pub async fn handle_install_command(args: InstallCliArgs) -> Result<()> {
    run_install(InstallOptions {
        no_launch: args.no_launch,
    })
    .await
}
