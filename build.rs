//! Dockertest-rs dynamic build dependenies.
//!
//! We will build all the dockerfiles listed in `dockerfiles` folder.

use anyhow::Error;
use std::process::Command;

const BUILD_IMAGES_ENV: &str = "DOCKERTEST_BUILD_TEST_IMAGES";

fn main() -> Result<(), Error> {
    println!("cargo:rerun-if-env-changed={}", BUILD_IMAGES_ENV);

    let build_enabled = std::env::var(BUILD_IMAGES_ENV)
        .map(|v| v == "1")
        .unwrap_or(false);

    if build_enabled {
        for entry in std::fs::read_dir("dockerfiles")? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                continue;
            }

            let pathbuf = entry.path();
            let file_stem = pathbuf.as_path().file_stem().expect("missing filename");
            let mut repository = std::ffi::OsString::from("dockertest-rs/");
            repository.push(file_stem);

            Command::new("docker")
                .current_dir("dockerfiles")
                .arg("build")
                .arg("-t")
                .arg(repository)
                .arg("-f")
                .arg(entry.file_name())
                .arg(".")
                .output()
                .expect("failed to build docker image");
        }
    }

    Ok(())
}
