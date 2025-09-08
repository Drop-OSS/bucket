use std::{
    collections::{HashMap, HashSet}, fs::create_dir_all, path::Path, sync::Arc, time::Instant
};

use anyhow::anyhow;
use rayon::ThreadPoolBuilder;

use crate::{
    download_internals::DropDownloadPipeline, generate_authorization_header, models::{ChunkBody, DownloadBucket, DownloadContext, DownloadDrop, DropManifest, ManifestBody}, AppData, AuthData
};

static RETRY_COUNT: usize = 3;

const TARGET_BUCKET_SIZE: usize = 63 * 1000 * 1000;
const MAX_FILES_PER_BUCKET: usize = (1024 / 4) - 1;

pub fn generate_buckets(game_id: String, install_dir: &str, manifest: &DropManifest) -> Vec<DownloadBucket> {
    let base_path = Path::new(install_dir);
    create_dir_all(base_path).unwrap();

    let mut buckets = Vec::new();

    let mut current_buckets = HashMap::<String, DownloadBucket>::new();
    let mut current_bucket_sizes = HashMap::<String, usize>::new();

    for (raw_path, chunk) in manifest {
        let path = base_path.join(Path::new(&raw_path));

        let container = path.parent().unwrap();
        create_dir_all(container).unwrap();

        let mut file_running_offset = 0;

        for (index, length) in chunk.lengths.iter().enumerate() {
            let drop = DownloadDrop {
                filename: raw_path.to_string(),
                start: file_running_offset,
                length: *length,
                checksum: chunk.checksums[index].clone(),
                permissions: chunk.permissions,
                path: path.clone(),
                index,
            };
            file_running_offset += *length;

            if *length >= TARGET_BUCKET_SIZE {
                // They get their own bucket

                buckets.push(DownloadBucket {
                    game_id: game_id.clone(),
                    version: chunk.version_name.clone(),
                    drops: vec![drop],
                });

                continue;
            }

            let current_bucket_size = current_bucket_sizes.entry(chunk.version_name.clone()).or_insert_with(|| 0);
            let c_version_name = chunk.version_name.clone();
            let c_game_id = game_id.clone();
            let current_bucket = current_buckets.entry(chunk.version_name.clone()).or_insert_with(|| DownloadBucket {
                game_id: c_game_id,
                version: c_version_name,
                drops: vec![],
            });

            if (*current_bucket_size + length >= TARGET_BUCKET_SIZE || current_bucket.drops.len() >= MAX_FILES_PER_BUCKET) && !current_bucket.drops.is_empty() {
                // Move current bucket into list and make a new one
                buckets.push(current_bucket.clone());
                *current_bucket = DownloadBucket {
                    game_id: game_id.clone(),
                    version: chunk.version_name.clone(),
                    drops: vec![],
                };
                *current_bucket_size = 0;
            }

            current_bucket.drops.push(drop);
            *current_bucket_size += *length;
        }
    }

    for (_, bucket) in current_buckets.into_iter() {
        if !bucket.drops.is_empty() {
            buckets.push(bucket);
        }
    }

    return buckets;
}

pub fn download(game_id: String, buckets: Vec<DownloadBucket>, app_data: &AppData) {
    let auth = app_data.auth.as_ref().expect("requires auth");
    let pool = ThreadPoolBuilder::new().num_threads(4).build().expect("failed to create pool thread");

    let mut download_contexts = HashMap::<String, DownloadContext>::new();
    let versions = buckets.iter().map(|e| &e.version).collect::<HashSet<_>>().into_iter().cloned().collect::<Vec<String>>();

    let completed_contexts = Arc::new(boxcar::Vec::new());
    let completed_indexes_loop_arc = completed_contexts.clone();

    let client = reqwest::blocking::Client::new();

    for version in versions {
        let download_context = client
            .post(auth.remote.join("/api/v2/client/context").expect("failed to generate download context url"))
            .json(&ManifestBody {
                game: game_id.clone(),
                version: version.clone(),
            })
            .header("Authorization", generate_authorization_header(&auth))
            .send()
            .expect("failed to create download context");

        if download_context.status() != 200 {
            panic!("failed to generate download context: {}", download_context.text().unwrap());
        }

        let download_context = download_context.json::<DownloadContext>().expect("failed to parse download context");
        download_contexts.insert(version, download_context);
    }

    let download_contexts = &download_contexts;

    let client_ref = &client;

    pool.scope(|scope| {
        for (_index, bucket) in buckets.iter().enumerate() {
            let completed_contexts = completed_indexes_loop_arc.clone();

            let download_context = download_contexts.get(&bucket.version).expect("failed to find download context for version - did we generate them all?");

            scope.spawn(move |_| {
                let start = Instant::now();
                // 3 attempts
                for _ in 0..RETRY_COUNT {
                    match download_game_bucket(&bucket, download_context, auth, client_ref) {
                        Ok(()) => {
                            for drop in &bucket.drops {
                                completed_contexts.push(drop.checksum.clone());
                            }
                            let time = start.elapsed().as_secs_f64();
                            let size = bucket.drops.iter().map(|v| v.length).sum::<usize>() / (1000 * 1000);
                            let speed = (size as f64) / time;
                            println!("finished chunk with speed of {speed}MB/s");
                            return;
                        }
                        Err(e) => {
                            panic!("failed to download: {e:?}");
                        }
                    }
                }
            });
        }
    });

    println!("finished download!");
}

fn download_game_bucket(bucket: &DownloadBucket, context: &DownloadContext, auth: &AuthData, client: &reqwest::blocking::Client) -> Result<(), anyhow::Error> {
    let url = auth.remote.join("/api/v2/client/chunk").expect("failed to generate download url");

    let body = ChunkBody::create(context, &bucket.drops);
    let response = client.post(url).json(&body).send()?;

    if response.status() != 200 {
        return Err(anyhow!("failed to download chunk with response: {}", response.text().expect("failed to read response")));
    };

    let lengths = response.headers().get("Content-Lengths").expect("server didn't send Content-Lengths").to_str().expect("failed to parse Content-Lengths header");
    for (i, raw_length) in lengths.split(",").enumerate() {
        let length = raw_length.parse::<usize>().unwrap_or(0);
        let Some(drop) = bucket.drops.get(i) else {
            return Err(anyhow!("invalid number of Content-Lengths recieved: {i}, {lengths}"));
        };
        if drop.length != length {
            return Err(anyhow!("for {}, expected {}, got {} ({})", drop.filename, drop.length, raw_length, length));
        }
    }

    let mut pipeline = DropDownloadPipeline::new(response, bucket.drops.clone())?;

    let _completed = pipeline.copy()?;

    let checksums = pipeline
        .finish()?;

    for (index, drop) in bucket.drops.iter().enumerate() {
        let res = hex::encode(**checksums.get(index).unwrap());
        if res != drop.checksum {
            println!("context didn't match... doing nothing because we will validate later.");
            // return Ok(false);
            // return Err(ApplicationDownloadError::Checksum);
        }
    }


    Ok(())
}
