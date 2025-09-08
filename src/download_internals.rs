use std::{
    fs::{File, OpenOptions},
    io::{self, BufWriter, Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

use md5::{Context, Digest};
use reqwest::blocking::Response;

use crate::models::DownloadDrop;

static MAX_PACKET_LENGTH: usize = 4096 * 4;
static BUMP_SIZE: usize = 4096 * 16;

pub struct DropWriter<W: Write> {
    hasher: Context,
    destination: BufWriter<W>,
}
impl DropWriter<File> {
    fn new(path: PathBuf) -> Result<Self, io::Error> {
        let destination = OpenOptions::new().write(true).create(true).truncate(false).open(&path)?;
        Ok(Self {
            destination: BufWriter::with_capacity(1024 * 1024, destination),
            hasher: Context::new(),
        })
    }

    fn finish(mut self) -> io::Result<Digest> {
        self.flush()?;
        Ok(self.hasher.finalize())
    }
}
// Write automatically pushes to file and hasher
impl Write for DropWriter<File> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.hasher.write_all(buf).map_err(|e| io::Error::other(format!("Unable to write to hasher: {e}")))?;
        let bytes_written = self.destination.write(buf)?;

        Ok(bytes_written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.hasher.flush()?;
        self.destination.flush()
    }
}
// Seek moves around destination output
impl Seek for DropWriter<File> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.destination.seek(pos)
    }
}

pub struct DropDownloadPipeline<R: Read, W: Write> {
    pub source: R,
    pub drops: Vec<DownloadDrop>,
    pub destination: Vec<DropWriter<W>>,
}

impl DropDownloadPipeline<Response, File> {
    pub fn new(source: Response, drops: Vec<DownloadDrop>) -> Result<Self, io::Error> {
        Ok(Self {
            source,
            destination: drops.iter().map(|drop| DropWriter::new(drop.path.clone())).try_collect()?,
            drops,
        })
    }

    pub fn copy(&mut self) -> Result<bool, io::Error> {
        let mut copy_buffer = [0u8; MAX_PACKET_LENGTH];
        for (index, drop) in self.drops.iter().enumerate() {
            let destination = self.destination.get_mut(index).ok_or(io::Error::other("no destination")).unwrap();
            let mut remaining = drop.length;
            if drop.start != 0 {
                destination.seek(SeekFrom::Start(drop.start.try_into().unwrap()))?;
            }
            let mut last_bump = 0;
            loop {
                let size = MAX_PACKET_LENGTH.min(remaining);
                let size = self.source.read(&mut copy_buffer[0..size]).inspect_err(|_| {
                    println!("got error from {}", drop.filename);
                })?;
                remaining -= size;
                last_bump += size;

                destination.write_all(&copy_buffer[0..size])?;

                if last_bump > BUMP_SIZE {
                    last_bump -= BUMP_SIZE;
                }

                if remaining == 0 {
                    break;
                };
            }
        }

        Ok(true)
    }

    #[allow(dead_code)]
    fn debug_skip_checksum(self) {
        self.destination.into_iter().for_each(|mut e| e.flush().unwrap());
    }

    pub fn finish(self) -> Result<Vec<Digest>, io::Error> {
        let checksums = self.destination.into_iter().map(|e| e.finish()).try_collect()?;
        Ok(checksums)
    }
}
