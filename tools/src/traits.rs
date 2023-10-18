use anyhow::Result;

pub trait CommandRunner {
    fn run(self) -> Result<()>;
}
