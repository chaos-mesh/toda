use futures::future::select_all;
use tokio::signal::unix::{signal, Signal, SignalKind};

pub struct Signals(Vec<Signal>);

impl Signals {
    pub fn from_kinds<'a>(
        kinds: impl 'a + IntoIterator<Item = &'a SignalKind>,
    ) -> anyhow::Result<Self> {
        let signals = kinds
            .into_iter()
            .map(|kind| signal(*kind))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Signals(signals))
    }

    pub async fn wait(&mut self) {
        select_all(self.0.iter_mut().map(|sig| Box::pin(sig.recv()))).await;
    }
}
