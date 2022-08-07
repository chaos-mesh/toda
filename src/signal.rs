use futures::future::select_all;
use tokio::signal::unix::{signal, Signal, SignalKind};
use std::path::PathBuf;

pub struct Signals
{
    pub signals :Vec<Signal>,
    interactive_path: PathBuf,
}

impl Signals {
    pub fn from_kinds<'a>(
        kinds: impl 'a + IntoIterator<Item = &'a SignalKind>,
        interactive_path: PathBuf,
    ) -> anyhow::Result<Self> {
        let signals = kinds
            .into_iter()
            .map(|kind| signal(*kind))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Signals{signals, interactive_path})
    }

    pub async fn wait(&mut self) -> anyhow::Result<()> {
        select_all(self.signals.iter_mut().map(|sig| Box::pin(sig.recv()))).await;
        if self.interactive_path != PathBuf::new() {
            std::fs::remove_file(self.interactive_path.clone()).unwrap();
        }
        Ok(())
    }
}
