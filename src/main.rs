mod torrent;

use anyhow::{anyhow, bail, Result};
use clap::Parser;
use rand::seq::SliceRandom;
use sha1_smol::Sha1;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    dir: String,

    torrent: String,
}

fn enumerate_files_with_sizes(dir: &str) -> HashMap<u64, Vec<PathBuf>> {
    let mut results = HashMap::<_, Vec<_>>::new();
    for entry in walkdir::WalkDir::new(dir) {
        let Ok(entry) = entry else {
            // TODO: error handling?
            continue;
        };
        // TODO: handle symlinks?
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            // TODO: error handling?
            continue;
        };
        results
            .entry(metadata.len())
            .or_default()
            .push(entry.into_path());
    }
    results
}

trait CheckWithFileMapping {
    fn check(&self, mapping: &HashMap<PathBuf, PathBuf>) -> Result<bool>;
}

impl CheckWithFileMapping for torrent::Piece {
    fn check(&self, mapping: &HashMap<PathBuf, PathBuf>) -> Result<bool> {
        println!("Checking {self:?}");
        let mut sha1 = Sha1::new();
        for slice in &self.file_slices {
            let file = File::open(
                mapping
                    .get(&slice.path)
                    .ok_or_else(|| anyhow!("no mapping for {:?}", slice.path))?,
            )?;
            let mut buffer = vec![0; slice.length.try_into()?];
            let bytes_read = rustix::io::pread(file, &mut buffer, slice.offset)?;
            if bytes_read as u64 != slice.length {
                bail!(
                    "pread failed for {:?}: read {} bytes at offset {} instead of {} bytes",
                    slice.path,
                    bytes_read,
                    slice.offset,
                    slice.length
                );
            }
            sha1.update(&buffer);
        }
        Ok(sha1.digest().bytes() == self.hash.bytes())
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let torrent: torrent::Torrent = serde_bencode::from_bytes(&std::fs::read(args.torrent)?)?;
    let entries = enumerate_files_with_sizes(&args.dir);
    println!("Matching torrent {}", torrent.info.name);
    // By definition, potential candidates must have matching file sizes.
    let candidates = if let Some(files) = torrent.info.files {
        files
            .iter()
            .map(|file| {
                let Some(entry) = entries.get(&file.length) else {
                    bail!(
                        "unable to find candidate matches for file {:?} with size {}",
                        file.path,
                        file.length
                    );
                };
                Ok((file.path.clone(), entry.clone()))
            })
            .collect::<Result<HashMap<_, _>, _>>()?
    } else {
        let path = torrent.info.name.into();
        let length = torrent
            .info
            .length
            .ok_or_else(|| anyhow!("single-file torrent without length set in info"))?;
        let entry = entries.get(&length).ok_or_else(|| {
            anyhow!(
                "unable to find candidate matches for file {:?} with size {}",
                path,
                length
            )
        })?;
        HashMap::from([(path, entry.clone())])
    };
    let mut path_to_pieces = HashMap::<_, Vec<_>>::new();
    for piece in &torrent.info.pieces {
        for slice in &piece.file_slices {
            path_to_pieces.entry(&slice.path).or_default().push(piece);
        }
    }
    // TODO: Implement an actual strategy. For now, just pick the last file as the candidate
    // because it's very simple.
    let candidates = candidates
        .into_iter()
        .map(|(path, mut candidates)| (path, candidates.pop().unwrap()))
        .collect::<HashMap<_, _>>();
    // TODO(dcheng): Implement a better piece strategy. For now, select up to 5 pieces per file.
    let pieces = path_to_pieces
        .into_iter()
        .map(|(path, mut pieces)| {
            let piece_count = std::cmp::min(5, pieces.len());
            pieces.shuffle(&mut rand::thread_rng());
            Vec::from(&pieces[..piece_count])
        })
        .flatten()
        .collect::<Vec<_>>();
    let mut failed_paths = HashSet::new();
    for piece in &pieces {
        if !piece.check(&candidates)? {
            failed_paths.extend(piece.file_slices.iter().map(|slice| &slice.path));
        }
    }
    if failed_paths.is_empty() {
        println!("Found a match!");
    } else {
        println!("Failed to match some paths: {failed_paths:?}");
    }
    Ok(())
}
