use bollard::volume::RemoveVolumeOptions;
use futures::future::join_all;
use tracing::{event, Level};

use super::Docker;

impl Docker {
    pub async fn remove_volumes(&self, volumes: &[String]) {
        join_all(
            volumes
                .iter()
                .map(|v| {
                    event!(Level::INFO, "removing named volume: {:?}", &v);
                    let options = Some(RemoveVolumeOptions { force: true });
                    self.client.remove_volume(v, options)
                })
                .collect::<Vec<_>>(),
        )
        .await;
    }
}
