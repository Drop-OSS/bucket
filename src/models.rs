use std::{collections::HashMap, path::PathBuf};

use clap::{Parser, arg};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitiateRequestBody {
    pub name: String,
    pub platform: String,
    pub capabilities: HashMap<String, ()>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct HandshakeRequestBody {
    pub client_id: String,
    pub token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandshakeResponse {
    pub private: String,
    pub certificate: String,
    pub id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameVersion {
    pub game_id: String,
    pub version_name: String,
}

impl std::ops::Deref for GameVersion {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.game_id
    }
}

pub type DropManifest = HashMap<String, DropChunk>;
#[derive(Serialize, Deserialize, Debug, Clone, Ord, PartialOrd, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DropChunk {
    pub permissions: u32,
    pub ids: Vec<String>,
    pub checksums: Vec<String>,
    pub lengths: Vec<usize>,
    pub version_name: String,
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// ID of game to download
    #[arg(short, long)]
    pub game: Option<String>,

    /// Version of game to download, defaults to latest
    #[arg(long, short = 'k')]
    pub game_version: Option<String>,

    #[arg(long, default_value_t = format!("./game"))]
    pub install_dir: String,

    #[arg(long, short)]
    pub silent: bool,
}

#[derive(Debug, Clone, Serialize)]
// Drops go in buckets
pub struct DownloadDrop {
    pub index: usize,
    pub filename: String,
    pub path: PathBuf,
    pub start: usize,
    pub length: usize,
    pub checksum: String,
    pub permissions: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadBucket {
    pub game_id: String,
    pub version: String,
    pub drops: Vec<DownloadDrop>,
}

#[derive(Deserialize)]
pub struct DownloadContext {
    pub context: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkBodyFile {
    filename: String,
    chunk_index: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkBody {
    pub context: String,
    pub files: Vec<ChunkBodyFile>,
}

#[derive(Serialize)]
pub struct ManifestBody {
    pub game: String,
    pub version: String,
}

impl ChunkBody {
    pub fn create(context: &DownloadContext, drops: &[DownloadDrop]) -> ChunkBody {
        Self {
            context: context.context.clone(),
            files: drops
                .iter()
                .map(|e| ChunkBodyFile {
                    filename: e.filename.clone(),
                    chunk_index: e.index,
                })
                .collect(),
        }
    }
}
