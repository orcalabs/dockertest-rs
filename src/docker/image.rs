use super::Docker;
use crate::{DockerTestError, Image, Source};
use bollard::{
    auth::DockerCredentials, errors::Error, image::CreateImageOptions, secret::CreateImageInfo,
};
use futures::StreamExt;
use tracing::{debug, event, Level};

impl Docker {
    /// Pulls the `Image` if neccessary.
    ///
    /// This function respects the `Image` Source and PullPolicy settings.
    pub async fn pull_image(
        &self,
        image: &Image,
        default_source: &Source,
    ) -> Result<(), DockerTestError> {
        let pull_source = match &image.source {
            None => default_source,
            Some(r) => r,
        };

        let exists = self.does_image_exist(image).await?;

        if image.should_pull(exists, pull_source)? {
            let auth = image.resolve_auth(pull_source)?;
            self.do_pull(image, auth).await?;
        }

        let image_id = self.get_image_id(image).await?;

        // FIXME: If we encounter a scenario where the image should not be pulled, we need to err
        // with appropriate information. Currently, it fails with the same error message as
        // other scenarios.
        image.set_id(image_id).await;
        Ok(())
    }

    /// Checks whether the image exists locally through attempting to inspect it.
    ///
    /// If docker daemon communication failed, we will also implicitly return false.
    async fn does_image_exist(&self, image: &Image) -> Result<bool, DockerTestError> {
        match self
            .client
            .inspect_image(&format!("{}:{}", image.repository, image.tag))
            .await
        {
            Ok(_) => Ok(true),
            Err(e) => match e {
                Error::DockerResponseServerError {
                    message: _,
                    status_code,
                } => {
                    if status_code == 404 {
                        Ok(false)
                    } else {
                        Err(DockerTestError::Daemon(e.to_string()))
                    }
                }
                _ => Err(DockerTestError::Daemon(e.to_string())),
            },
        }
    }

    // Pulls the image from its source
    // NOTE(lint): uncertain how to structure this otherwise
    #[allow(clippy::match_single_binding)]
    async fn do_pull(
        &self,
        image: &Image,
        auth: Option<DockerCredentials>,
    ) -> Result<(), DockerTestError> {
        debug!("pulling image: {}:{}", image.repository, image.tag);
        let options = Some(CreateImageOptions::<&str> {
            from_image: &image.repository,
            tag: &image.tag,
            ..Default::default()
        });

        let mut stream = self.client.create_image(options, None, auth);
        // This stream will intermittently yield a progress update.
        while let Some(result) = stream.next().await {
            match result {
                Ok(intermitten_result) => match intermitten_result {
                    CreateImageInfo {
                        id,
                        error,
                        error_detail,
                        status,
                        progress,
                        progress_detail,
                    } => {
                        if error.is_some() {
                            event!(
                                Level::ERROR,
                                "pull error {} {:?}",
                                error.clone().unwrap(),
                                error_detail.unwrap_or_default()
                            );
                        } else {
                            event!(
                                Level::TRACE,
                                "pull progress {} {:?} {:?} {:?}",
                                status.clone().unwrap_or_default(),
                                id.clone().unwrap_or_default(),
                                progress.clone().unwrap_or_default(),
                                progress_detail.clone().unwrap_or_default()
                            );
                        }
                    }
                },
                Err(e) => {
                    let msg = match e {
                        Error::DockerResponseServerError {
                            message: _,
                            status_code,
                        } => {
                            if status_code == 404 {
                                "unknown registry or image".to_string()
                            } else {
                                e.to_string()
                            }
                        }
                        _ => e.to_string(),
                    };
                    return Err(DockerTestError::Pull {
                        repository: image.repository.to_string(),
                        tag: image.tag.to_string(),
                        error: msg,
                    });
                }
            }
        }

        // TODO: Verify that we have actually pulled the image.
        // NOTE: The engine may return a 500 when we unauthorized, but bollard does not give us
        // this failure. Rather, it just does not provide anthing in the stream.
        // If a repo is submitted that we do not have access to, and no auth is supplied,
        // we will no error.

        event!(Level::DEBUG, "successfully pulled image");
        Ok(())
    }

    async fn get_image_id(&self, image: &Image) -> Result<String, DockerTestError> {
        match self
            .client
            .inspect_image(&format!("{}:{}", image.repository, image.tag))
            .await
        {
            Ok(details) => Ok(details.id.expect("image did not have an id")),
            Err(e) => {
                event!(
                    Level::TRACE,
                    "failed to retrieve ID of image: {}, tag: {}, source: {:?} ",
                    image.repository,
                    image.tag,
                    image.source
                );
                Err(DockerTestError::Pull {
                    repository: image.repository.to_string(),
                    tag: image.tag.to_string(),
                    error: e.to_string(),
                })
            }
        }
    }
}
