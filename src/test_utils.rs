use crate::DockerTestError;

use access_queue::AccessQueue;
use bollard::{
    container::{ListContainersOptions, RemoveContainerOptions},
    errors::Error,
    image::RemoveImageOptions,
    Docker,
};
use futures::future::FutureExt;
use once_cell::sync::Lazy;
use std::collections::HashMap;

/// This is used to guard concurrent tests with pull logic.
pub(crate) static CONCURRENT_IMAGE_ACCESS_QUEUE: Lazy<AccessQueue<&str>> =
    Lazy::new(|| AccessQueue::new("williamyeh/dummy", 1));

/*
// Helper function that pulls the given image
// if its not present on the local system
pub(crate) fn pull_if_not_present<'a>(
    repository: &'a str,
    tag: &'a str,
    client: &'a Rc<shiplift::Docker>,
) -> impl Future<Item = String, Error = DockerTestError> + 'a {
    image_exists_locally(&repository, &tag, &client)
        .and_then(move |exists| {
            if !exists {
                let pull_fut = pull_image(&repository, &tag, &client)
                    .and_then(move |_| image_id(&repository, &tag, &client));
                future::Either::A(pull_fut)
            } else {
                future::Either::B(image_id(&repository, &tag, &client))
            }
        })
        .map(move |id| id)
}
*/

/// Helper function that deletes the given image if it exists locally.
pub(crate) async fn delete_image_if_present(
    client: &Docker,
    repository: &str,
    tag: &str,
) -> Result<(), DockerTestError> {
    if image_exists_locally(client, repository, tag).await? {
        let id = image_id(client, repository, tag).await?;
        delete_image(client, id).await?;
    }

    Ok(())
}

/// Helper function querying if the repository image exists locally.
/// This will only error on daemon error.
pub(crate) async fn image_exists_locally(
    client: &Docker,
    repository: &str,
    tag: &str,
) -> Result<bool, DockerTestError> {
    client
        .inspect_image(&format!("{}:{}", repository, tag))
        .map(|result| match result {
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
        })
        .await
}

/// Helper function that removes a given image from the local system.
/// Useful when verifying image pull mechanisms.
pub(crate) async fn delete_image(client: &Docker, image_id: String) -> Result<(), DockerTestError> {
    // If remove containers fail, this image remove might catch it
    let _ = remove_containers(client, image_id.clone()).await;

    let options = RemoveImageOptions {
        force: true,
        ..Default::default()
    };
    client
        .remove_image(&image_id, Some(options), None)
        .await
        .map_err(|e| DockerTestError::Daemon(e.to_string()))?;
    Ok(())
}

/// Helper function that removes all containers that stems from a given image id.
pub(crate) async fn remove_containers(
    client: &Docker,
    image_id: String,
) -> Result<(), DockerTestError> {
    let mut filters = HashMap::new();
    filters.insert("ancestor", vec![image_id.as_str()]);

    let options = ListContainersOptions {
        all: true,
        filters: filters,
        ..Default::default()
    };

    let containers = client
        .list_containers(Some(options))
        .await
        .map_err(|e| DockerTestError::Daemon(e.to_string()))?;
    for container in containers {
        // force remove each container
        let options = RemoveContainerOptions {
            force: true,
            ..Default::default()
        };
        client
            .remove_container(&container.id.unwrap(), Some(options))
            .await
            .map_err(|e| DockerTestError::Daemon(e.to_string()))?;
    }

    Ok(())
}

/*
// Helper function that pulls a given image with the given tag
pub(crate) fn pull_image(
    repository: &str,
    tag: &str,
    client: &Rc<shiplift::Docker>,
) -> impl Future<Item = (), Error = DockerTestError> {
    let images = shiplift::Images::new(&client);
    let opts = PullOptions::builder().image(repository).tag(tag).build();

    images
        .pull(&opts)
        .collect()
        .map(|_| ())
        .map_err(|e| DockerTestError::Processing(format!("failed to pull image: {}", e)))
}
*/

/// Helper function that retreives the image id of a given Image.
pub(crate) async fn image_id(
    client: &Docker,
    repository: &str,
    tag: &str,
) -> Result<String, DockerTestError> {
    client
        .inspect_image(&format!("{}:{}", repository, tag))
        .await
        .map_err(|e| DockerTestError::Processing(format!("failed to get image id {}", e)))
        .map(|res| res.id.unwrap())
}

/*
pub(crate) async fn is_container_running(
    client: &Docker,
    container_id: String,
) -> Result<bool, DockerTestError> {
    let mut filters = HashMap::new();
    filters.insert("id", vec![container_id.as_str()]);
    filters.insert("status", vec!["running"]);

    let options = ListContainersOptions {
        all: true,
        filters: filters,
        ..Default::default()
    };

    let containers = client
        .list_containers(Some(options))
        .await
        .map_err(|e| DockerTestError::Daemon(e.to_string()))?;

    for c in containers {
        // should be unneccesary
        if c.id == container_id {
            return Ok(true);
        }
    }

    Ok(false)
}
*/
