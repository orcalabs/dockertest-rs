use failure::{format_err, Error};
use futures::future::{self, Future};
use futures::stream::Stream;
use shiplift;
use shiplift::builder::{ContainerListOptions, PullOptions, RmContainerOptions};
use std::rc::Rc;

// Helper function that pulls the given image
// if its not present on the local system
pub(crate) fn pull_if_not_present<'a>(
    repository: &'a str,
    tag: &'a str,
    client: &'a Rc<shiplift::Docker>,
) -> impl Future<Item = String, Error = Error> + 'a {
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

// Helper function that deletes the given image
// if it exists locally
pub(crate) fn delete_image_if_present<'a>(
    repository: &'a str,
    tag: &'a str,
    client: &'a Rc<shiplift::Docker>,
) -> impl Future<Item = (), Error = Error> + 'a {
    image_exists_locally(&repository, &tag, &client)
        .and_then(move |exists| {
            println!("image existence: {}", exists);
            if exists {
                let delete_fut = image_id(&repository, &tag, &client)
                    .and_then(move |id| delete_image(id, &client));
                future::Either::A(delete_fut)
            } else {
                future::Either::B(future::ok(()))
            }
        })
        .map(|_| ())
}

// Helper functions that checks wether the given image name exists on the local system
pub(crate) fn image_exists_locally(
    repository: &str,
    tag: &str,
    client: &Rc<shiplift::Docker>,
) -> impl Future<Item = bool, Error = Error> {
    client
        .images()
        .get(&format!("{}:{}", &repository, &tag))
        .inspect()
        .then(|res| match res {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        })
        .map(|res| res)
}

// Helper function that removes a given image from
// the local system, useful when verifying image
// pull mechanisms
pub(crate) fn delete_image<'a>(
    image_id: String,
    client: &'a Rc<shiplift::Docker>,
) -> impl Future<Item = (), Error = Error> + 'a {
    remove_containers(image_id.to_string(), &client).and_then(move |_| {
        client
            .images()
            .get(&image_id)
            .delete()
            .map_err(|e| format_err!("failed to delete existing image: {}", e))
            .map(|_| ())
    })
}

// Helper function that removes all containers that
// stems from a given image.
// Id represents the Image Id
pub(crate) fn remove_containers<'a>(
    image_id: String,
    client: &'a shiplift::Docker,
) -> impl Future<Item = (), Error = Error> + 'a {
    client
        .containers()
        .list(&ContainerListOptions::builder().all().build())
        .and_then(move |containers| {
            let mut fut_vec = Vec::new();
            for c in containers {
                if c.image == image_id {
                    let container_interface = shiplift::Container::new(&client, c.id);
                    let opts = RmContainerOptions::builder().force(true).build();
                    fut_vec.push(container_interface.remove(opts));
                }
            }

            future::join_all(fut_vec).map(|_| ())
        })
        .map_err(|e| format_err!("failed to remove containers: {}", e))
}

// Helper function that pulls a given image with the given tag
pub(crate) fn pull_image(
    repository: &str,
    tag: &str,
    client: &Rc<shiplift::Docker>,
) -> impl Future<Item = (), Error = Error> {
    let images = shiplift::Images::new(&client);
    let opts = PullOptions::builder().image(repository).tag(tag).build();

    images
        .pull(&opts)
        .collect()
        .map(|_| ())
        .map_err(|e| format_err!("failed to pull image: {}", e))
}

// Helper function that retreives the id of a given image
pub(crate) fn image_id(
    repository: &str,
    tag: &str,
    client: &Rc<shiplift::Docker>,
) -> impl Future<Item = String, Error = Error> {
    client
        .images()
        .get(&format!("{}:{}", repository, tag))
        .inspect()
        .map(|res| res.id)
        .map_err(|e| format_err!("failed to get image id {}", e))
}

pub(crate) fn is_container_running(
    container_id: String,
    client: &Rc<shiplift::Docker>,
) -> impl Future<Item = bool, Error = Error> {
    client
        .containers()
        .list(&ContainerListOptions::builder().build())
        .and_then(move |containers| {
            for c in containers {
                if c.id == container_id {
                    return future::ok(true);
                }
            }
            return future::ok(false);
        })
        .map_err(|e| format_err!("failed to remove containers: {}", e))
}
