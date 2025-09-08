#![feature(iterator_try_collect)]

use std::{
    collections::HashMap,
    env, fs,
    io::{self, BufRead},
};

use chrono::Utc;
use clap::Parser;
use droplet_rs::ssl::sign_nonce;
use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::{
    download::{download, generate_buckets},
    models::{Args, DropManifest, GameVersion, HandshakeRequestBody, HandshakeResponse, InitiateRequestBody},
};

#[derive(Serialize, Deserialize)]
struct AuthData {
    remote: Url,
    private: String,
    public: String,
    client_id: String,
}

#[derive(Serialize, Deserialize)]
struct AppData {
    auth: Option<AuthData>,
}

mod download;
mod models;
mod download_internals;

const APP_DATA_PATH: &'static str = "./bucket.json";

fn read_app_data() -> AppData {
    if fs::exists(APP_DATA_PATH).expect("failed to check for bucket.json") {
        let contents = fs::read_to_string(APP_DATA_PATH).expect("failed to read bucket.json");
        let app_data = serde_json::from_str::<AppData>(&contents).expect("failed to parse bucket.json");
        return app_data;
    };

    AppData { auth: None }
}

fn save_app_data(app_data: &AppData) {
    fs::write(APP_DATA_PATH, serde_json::to_string(app_data).expect("failed to serialize app_data")).expect("failed to save app data");
}

fn shitty_write<T>(lock: &mut T, value: String)
where
    T: io::Write,
{
    lock.write_all(value.as_bytes()).unwrap();
    lock.flush().unwrap();
}

fn do_auth(app_data: &mut AppData) {
    let mut lines = io::stdin().lock().lines();
    let mut stdout_lock = io::stdout().lock();
    shitty_write(&mut stdout_lock, format!("drop server url: "));
    let server_url = Url::parse(&lines.next().unwrap().unwrap()).expect("failed to parse url");

    let endpoint = server_url.join("/api/v1/client/auth/initiate").expect("failed to create initiate endpoint");
    let body = InitiateRequestBody {
        name: format!("bucket-cli"),
        platform: env::consts::OS.to_string(),
        capabilities: HashMap::new(),
    };

    let client = reqwest::blocking::Client::new();
    let response = client.post(endpoint).json(&body).send().expect("failed to initiate auth");

    let mut callback = response.text().expect("failed to read callback url");
    shitty_write(&mut stdout_lock, format!("open {}{} in your browser...\n", server_url, callback.split_off(1)));

    shitty_write(&mut stdout_lock, "handshake response: ".to_owned());
    let handshake = lines.next().unwrap().unwrap();

    let handshake = handshake.split('/').collect::<Vec<&str>>();
    if handshake.len() != 2 {
        panic!("handshake is expected to be in format .../...");
    }

    let client_id = handshake.get(0).expect("failed to fetch client id from handshake");
    let token = handshake.get(1).expect("failed to fetch token from handshake");

    let body = HandshakeRequestBody {
        client_id: (*client_id).to_string(),
        token: (*token).to_string(),
    };
    let endpoint = server_url.join("/api/v1/client/auth/handshake").expect("failed to make handshake url");
    let response = client.post(endpoint).json(&body).send().expect("failed to complete handshake");

    if response.status() != 200 {
        panic!("handshake failed with: {}", response.text().expect("failed to read handshake response"));
    }

    let response = response.json::<HandshakeResponse>().expect("failed to parse handshake response");

    app_data.auth = Some(AuthData {
        remote: server_url,
        private: response.private,
        public: response.certificate,
        client_id: response.id,
    });
}

fn fetch_params(args: &mut Args) -> (String, String) {
    if let Some(game) = &args.game
        && args.silent
    {
        return (game.to_string(), args.game_version.clone().unwrap_or("".to_owned()));
    };

    if args.silent {
        panic!("silent mode set, but game not specified")
    };

    let mut lines = io::stdin().lock().lines();
    let mut stdout_lock = io::stdout().lock();

    loop {
        shitty_write(&mut stdout_lock, format!("game ID [{}]: ", args.game.clone().unwrap_or("<unset>".to_string())));
        let game_id = lines.next().unwrap().unwrap();
        if game_id.len() != 0 {
            args.game = Some(game_id)
        }

        if args.game.is_some() {
            break;
        }
    }

    shitty_write(&mut stdout_lock, format!("game version [{}]: ", args.game_version.clone().unwrap_or("<latest>".to_string())));
    let game_version = lines.next().unwrap().unwrap();
    if game_version.len() != 0 {
        args.game_version = Some(game_version);
    };

    return (args.game.clone().unwrap().to_string(), args.game_version.clone().unwrap_or("".to_owned()));
}

fn generate_authorization_header(certs: &AuthData) -> String {
    let nonce = Utc::now().timestamp_millis().to_string();

    let signature = sign_nonce(certs.private.clone(), nonce.clone()).unwrap();

    format!("Nonce {} {} {}", certs.client_id, nonce, signature)
}

fn discover_latest_version(game_id: &str, auth: &AuthData) -> String {
    let endpoint = auth.remote.join(&format!("/api/v1/client/game/versions?id={}", game_id)).expect("failed to build discovery url");
    let client = reqwest::blocking::Client::new();
    let response = client.get(endpoint).header("Authorization", generate_authorization_header(auth)).send().expect("failed to discover versions");

    let versions = response.json::<Vec<GameVersion>>().expect("failed to parse versions");

    let version = versions.get(0).expect("no versions available for game").version_name.clone();

    println!("found \"{}\" as latest version", version);

    return version;
}

fn fetch_manifest(params: (String, String), app_data: &AppData) -> DropManifest {
    println!("downloading game manifest...");

    let auth = app_data.auth.as_ref().expect("required auth data");

    let version = { if params.1.len() == 0 { discover_latest_version(&params.0, auth) } else { params.1 } };

    let url = auth.remote.join(&format!("/api/v1/client/game/manifest?id={}&version={}", params.0, version)).expect("failed to create manifest URL");
    let client = reqwest::blocking::Client::new();
    let response = client.get(url).header("Authorization", generate_authorization_header(auth)).send().expect("failed to fetch manifest");

    if response.status() != 200 {
        panic!("failed to fetch manifest: {}", response.text().expect("failed to read manifest error"));
    }

    let manifest = response.json::<DropManifest>().expect("failed to parse manifest");

    return manifest;
}

fn main() {
    let mut args = Args::parse();

    let mut app_data = read_app_data();

    while app_data.auth.is_none() {
        do_auth(&mut app_data);
    }
    save_app_data(&app_data);

    let params = fetch_params(&mut args);

    println!("downloading GAMEID: {}, VERSION: {}", params.0, params.1);

    println!("fetching manifest...");
    let manifest = fetch_manifest(params.clone(), &app_data);
    println!("downloaded manifest");

    println!("generating buckets...");
    let buckets = generate_buckets(params.0.clone(), &args.install_dir, &manifest);
    println!("generated {} buckets", buckets.len());

    println!("downloading game...");
    download(params.0, buckets, &app_data, &args);
}
