mod fs;
mod torrent;
mod util;

use anyhow::{anyhow, bail, Result};
use clap::Parser;
use indicatif::ProgressIterator;
use rand::seq::SliceRandom;
use sha1_smol::Sha1;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    source_dir: PathBuf,

    #[arg(long)]
    target_dir: PathBuf,

    #[arg(long)]
    dry_run: bool,

    torrent: String,
}

fn enumerate_files_with_sizes(dir: &Path) -> HashMap<u64, Vec<PathBuf>> {
    let mut results = HashMap::<_, Vec<_>>::new();
    let bar = util::new_spinner().with_message(format!("enumerating files in {}", dir.display()));
    bar.enable_steady_tick(std::time::Duration::from_millis(125));
    for entry in walkdir::WalkDir::new(dir).into_iter().progress_with(bar) {
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
        let mut sha1 = Sha1::new();
        for slice in &self.file_slices {
            let file = File::open(
                mapping
                    .get(&slice.path)
                    .ok_or_else(|| anyhow!("no mapping for {}", slice.path.display()))?,
            )?;
            let mut buffer = vec![0; slice.length.try_into()?];
            let bytes_read = rustix::io::pread(file, &mut buffer, slice.offset)?;
            if bytes_read as u64 != slice.length {
                bail!(
                    "pread failed for {}: read {} bytes at offset {} instead of {} bytes",
                    slice.path.display(),
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
    println!("parsed torrent {}", torrent.info.name);
    let entries = enumerate_files_with_sizes(&args.source_dir);
    // By definition, potential candidates must have matching file sizes.
    let candidates = if let Some(files) = torrent.info.files {
        files
            .iter()
            .map(|file| {
                let Some(entry) = entries.get(&file.length) else {
                    bail!(
                        "unable to find candidate matches for file {} with size {}",
                        file.path.display(),
                        file.length
                    );
                };
                Ok((file.path.clone(), entry.clone()))
            })
            .collect::<Result<HashMap<_, _>, _>>()?
    } else {
        let path: PathBuf = torrent.info.name.clone().into();
        let length = torrent
            .info
            .length
            .ok_or_else(|| anyhow!("single-file torrent without length set in info"))?;
        let entry = entries.get(&length).ok_or_else(|| {
            anyhow!(
                "unable to find candidate matches for file {} with size {}",
                path.display(),
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
        .iter_mut()
        .flat_map(|(_path, pieces)| {
            let piece_count = std::cmp::min(5, pieces.len());
            pieces.shuffle(&mut rand::thread_rng());
            &pieces[..piece_count]
        })
        .collect::<Vec<_>>();
    let mut failed_paths = HashSet::new();
    let bar = util::new_bar(pieces.len() as u64).with_message("hashing...");
    for piece in pieces.iter().progress_with(bar) {
        if !piece.check(&candidates)? {
            failed_paths.extend(piece.file_slices.iter().map(|slice| &slice.path));
        }
    }
    if !failed_paths.is_empty() {
        bail!("failed to match some paths: {failed_paths:?}");
    }
    if torrent.info.length.is_some() {
        bail!("cross-seed setup is not yet supported for single-file torrents");
    }
    let base_dir: PathBuf = [
        args.target_dir,
        url::Url::parse(&torrent.announce)?
            .host_str()
            .ok_or_else(|| anyhow!("announce URL {} has no hostname", torrent.announce))?
            .into(),
        torrent.info.name.clone().into(),
    ]
    .iter()
    .collect();
    let fs = if args.dry_run {
        fs::get_dry_run_instance()
    } else {
        fs::get_default_instance()
    };
    fs.create_dir_all(&base_dir)?;
    for (source_path, target_path) in &candidates {
        if let Some(parent) = source_path.parent() {
            fs.create_dir_all(&base_dir.join(parent))?;
        }
        fs.symlink(target_path, &base_dir.join(source_path))?;
    }
    // TODO: Automatically add it in paused mode to the torrent client.
    Ok(())
}
