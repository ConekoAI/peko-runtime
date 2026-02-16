//! CLI channel

use super::Channel;

/// Command line interface channel
pub struct CliChannel;

impl CliChannel {
    pub fn new() -> Self {
        Self
    }
}

impl Channel for CliChannel {
    fn name(&self) -> &str {
        "cli"
    }

    async fn send(&self, message: &str) -> anyhow::Result<()> {
        println!("{}", message);
        Ok(())
    }

    async fn receive(&mut self) -> anyhow::Result<Option<String>> {
        // TODO: Read from stdin
        Ok(None)
    }
}
