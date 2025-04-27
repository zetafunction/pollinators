mod client;
mod fs;
mod torrent;
mod util;

use anyhow::{anyhow, bail, Result};
use clap::Parser;
use console::style;
use indicatif::ProgressIterator;
use rand::seq::SliceRandom;
use sha1_smol::Sha1;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};

#[derive(Parser)]
struct Args {
    /// Where to look for potential matches. May be specified multiple times.
    #[arg(long)]
    source_dir: Vec<PathBuf>,

    /// Where to create the symlinks, if needed.
    #[arg(long)]
    target_dir: PathBuf,

    /// If true, only prints out the changes that would have been made.
    #[arg(long)]
    dry_run: bool,

    /// How many pieces should be tested per file when checking for a match.
    #[arg(long, default_value_t = 3)]
    pieces_to_test: usize,

    torrents: Vec<PathBuf>,
}

fn enumerate_files_with_sizes<P: AsRef<Path>>(dirs: &[P]) -> HashMap<u64, Vec<PathBuf>> {
    let mut results = HashMap::<_, Vec<_>>::new();
    let bar = util::new_spinner();
    bar.enable_steady_tick(std::time::Duration::from_millis(125));
    for dir in dirs {
        bar.set_message(format!("enumerating {}", dir.as_ref().display()));
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
    }
    bar.finish_with_message(format!("enumerated {} files", results.len()));
    results
}

trait CheckWithFileMapping {
    fn check(&self, mapping: &HashMap<&PathBuf, &PathBuf>) -> Result<bool>;
}

impl CheckWithFileMapping for torrent::Piece {
    fn check(&self, mapping: &HashMap<&PathBuf, &PathBuf>) -> Result<bool> {
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

trait CrossSeed {
    fn base_dir(&self, target_dir: &Path) -> Result<PathBuf>;
    fn cross_seed(
        &self,
        dry_run: bool,
        path: &Path,
        target_dir: &Path,
        candidates: &HashMap<&PathBuf, &PathBuf>,
    ) -> Result<()>;
}

impl CrossSeed for torrent::Torrent {
    fn base_dir(&self, target_dir: &Path) -> Result<PathBuf> {
        Ok(target_dir.join(
            url::Url::parse(&self.announce)?
                .host_str()
                .ok_or_else(|| anyhow!("announce URL {} has no hostname", self.announce))?,
        ))
    }

    fn cross_seed(
        &self,
        dry_run: bool,
        path: &Path,
        target_dir: &Path,
        candidates: &HashMap<&PathBuf, &PathBuf>,
    ) -> Result<()> {
        if self.info.is_single_file {
            let (source, target) = candidates.iter().next().unwrap();
            return if *source == target.file_name().unwrap() {
                client::new_instance(dry_run).add_torrent(path, target.parent().unwrap())
            } else {
                let base_dir = self.base_dir(target_dir)?;
                let fs = fs::new_instance(dry_run);
                fs.create_dir_all(&base_dir)?;
                fs.symlink(target, &base_dir.join(source))?;
                client::new_instance(dry_run).add_torrent(path, &base_dir)
            };
        }

        // Check if symlinks are needed at all. This could be much simpler if Path implemented
        // strip_suffix, but for whatever reason, Path only implements strip_prefix.
        let path_prefix: HashSet<Option<PathBuf>> = candidates
            .iter()
            .map(|(source, target)| {
                let mut source_components = source.components().rev();
                let mut target_components = target.components().rev();
                loop {
                    match (source_components.next(), target_components.next()) {
                        (Some(s), Some(t)) if s == t => continue,
                        (None, Some(t)) => {
                            return Some(target_components.rev().chain(Some(t)).collect());
                        }
                        _ => return None,
                    }
                }
            })
            .collect();
        if !path_prefix.contains(&None) && path_prefix.len() == 1 {
            let seed_path = path_prefix.into_iter().next().unwrap().unwrap();
            client::new_instance(dry_run).add_torrent(path, &seed_path)?;
            return Ok(());
        }
        let base_dir = self.base_dir(target_dir)?;
        println!(
            "{}",
            style("found matches with different filenames; creating symlinks").blue()
        );
        let fs = fs::new_instance(dry_run);
        for (source_path, target_path) in candidates {
            if let Some(parent) = source_path.parent() {
                fs.create_dir_all(&base_dir.join(parent))?;
            }
            fs.symlink(target_path, &base_dir.join(source_path))?;
        }
        client::new_instance(dry_run).add_torrent(path, &base_dir)?;

        Ok(())
    }
}

fn process_torrent(
    path: &Path,
    target_dir: &Path,
    entries: &HashMap<u64, Vec<PathBuf>>,
    pieces_to_test: usize,
    dry_run: bool,
) -> Result<()> {
    let torrent: torrent::Torrent = serde_bencode::from_bytes(&std::fs::read(path)?)?;
    println!("processing {}", path.display());
    // By definition, potential candidates must have matching file sizes.
    let candidates = torrent
        .info
        .files
        .iter()
        .map(|file| {
            let Some(entry) = entries.get(&file.length) else {
                bail!(
                    "unable to find candidate matches for file {} with size {}",
                    file.path.display(),
                    file.length
                );
            };
            Ok((&file.path, entry))
        })
        .collect::<Result<HashMap<_, _>, _>>()?;
    let mut path_to_pieces = HashMap::<_, Vec<_>>::new();
    for piece in &torrent.info.pieces {
        for slice in &piece.file_slices {
            path_to_pieces.entry(&slice.path).or_default().push(piece);
        }
    }
    // TODO: This doesn't prevent duplicate assignments, which is probably not desirable.
    let candidates = candidates
        .into_iter()
        .map(|(path, candidates)| {
            let candidate = candidates
                .iter()
                .map(|candidate| {
                    let common_suffix = candidate
                        .iter()
                        .rev()
                        .zip(path.iter().rev())
                        .take_while(|(x, y)| x == y)
                        .count();
                    (common_suffix, candidate)
                })
                .max()
                .unwrap();
            (path, candidate.1)
        })
        .collect::<HashMap<_, _>>();
    // Sample a (configurable) number of pieces to file as a quick correctness check.
    let pieces = path_to_pieces
        .iter_mut()
        .flat_map(|(_path, pieces)| {
            let piece_count = std::cmp::min(pieces_to_test, pieces.len());
            pieces.shuffle(&mut rand::thread_rng());
            &pieces[..piece_count]
        })
        .collect::<HashSet<_>>();
    let mut failed_paths = HashSet::new();
    let bar = util::new_bar(pieces.len() as u64).with_message("hashing...");
    for piece in pieces.iter().progress_with(bar) {
        if !piece.check(&candidates)? {
            failed_paths.extend(piece.file_slices.iter().map(|slice| &slice.path));
        }
    }
    if !failed_paths.is_empty() {
        bail!("hash check failed for paths: {failed_paths:?}");
    }

    torrent.cross_seed(dry_run, path, target_dir, &candidates)
}

fn main() -> Result<()> {
    let args = Args::parse();
    let entries = enumerate_files_with_sizes(&args.source_dir);
    for torrent in args.torrents {
        if let Err(err) = process_torrent(
            &torrent,
            &args.target_dir,
            &entries,
            args.pieces_to_test,
            args.dry_run,
        ) {
            println!("{} {:?}", style("error:").red(), style(err).red());
        }
    }
    Ok(())
}
