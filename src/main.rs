mod client;
mod fs;
mod torrent;
mod util;

use anyhow::{anyhow, bail, Result};
use clap::Parser;
use console::style;
use indicatif::ParallelProgressIterator;
use rand::seq::SliceRandom;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
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

    /// If true, skips adding the torrent to a client.
    #[arg(long)]
    skip_add: bool,

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
    fn check(&self, mapping: &HashMap<&Path, &Path>) -> Result<bool>;
}

impl CheckWithFileMapping for torrent::Piece {
    fn check(&self, mapping: &HashMap<&Path, &Path>) -> Result<bool> {
        let mut sha1 = Sha1::new();
        for slice in &self.file_slices {
            let file = File::open(
                mapping
                    .get::<Path>(slice.path.as_ref())
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
        skip_add: bool,
        path: &Path,
        target_dir: &Path,
        candidates: &HashMap<&Path, &Path>,
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
        skip_add: bool,
        path: &Path,
        target_dir: &Path,
        candidates: &HashMap<&Path, &Path>,
    ) -> Result<()> {
        if self.info.is_single_file {
            let (source, target) = candidates.iter().next().unwrap();
            return if *source == target.file_name().unwrap() {
                println!(
                    "torrent can be directly seeded from {}",
                    target.parent().unwrap().display()
                );
                if !skip_add {
                    client::new_instance(dry_run).add_torrent(path, target.parent().unwrap())?;
                }
                Ok(())
            } else {
                let base_dir = self.base_dir(target_dir)?;
                println!(
                    "{} {}",
                    style("found matches with different filenames; creating symlinks in").blue(),
                    base_dir.display()
                );
                let fs = fs::new_instance(dry_run);
                fs.create_dir_all(&base_dir)?;
                fs.symlink(target, &base_dir.join(source))?;
                if !skip_add {
                    client::new_instance(dry_run).add_torrent(path, &base_dir)
                } else {
                    Ok(())
                }
            };
        }

        // Check if symlinks are needed at all; if the same prefix can be used for all selected
        // candidate paths, then a symlink is sufficient.
        let path_prefix: HashSet<Option<PathBuf>> = candidates
            .iter()
            .map(|(source, target)| target.remove_common_suffix(source))
            .collect();
        if !path_prefix.contains(&None) && path_prefix.len() == 1 {
            let seed_path = path_prefix.into_iter().next().unwrap().unwrap();
            println!(
                "torrent can be directly seeded from {}",
                seed_path.display()
            );
            if !skip_add {
                client::new_instance(dry_run).add_torrent(path, &seed_path)?;
            }
            return Ok(());
        }
        let base_dir = self.base_dir(target_dir)?;
        println!(
            "{} {}",
            style("found matches with different filenames; creating symlinks in").blue(),
            base_dir.display()
        );
        let fs = fs::new_instance(dry_run);
        for (source_path, target_path) in candidates {
            if let Some(parent) = source_path.parent() {
                fs.create_dir_all(&base_dir.join(parent))?;
            }
            fs.symlink(target_path, &base_dir.join(source_path))?;
        }
        if !skip_add {
            client::new_instance(dry_run).add_torrent(path, &base_dir)?;
        }

        Ok(())
    }
}

trait PathHelper {
    fn remove_common_suffix(&self, suffix: &Self) -> Option<PathBuf>;
}

impl PathHelper for Path {
    fn remove_common_suffix(&self, suffix: &Path) -> Option<PathBuf> {
        let mut self_components = self.components().rev();
        let mut suffix_components = suffix.components().rev();
        loop {
            match (self_components.next(), suffix_components.next()) {
                (Some(x), Some(y)) if x == y => continue,
                (Some(x), None) => {
                    return Some(self_components.rev().chain(Some(x)).collect());
                }
                _ => return None,
            }
        }
    }
}

fn get_best_candidate<'a, P, Q>(
    path: &'a Path,
    candidates: &'a [P],
    preferred_prefix: &Option<Q>,
) -> Option<(&'a Path, &'a Path)>
where
    P: AsRef<Path> + Ord,
    Q: AsRef<Path>,
{
    let candidate = candidates
        .iter()
        .map(|candidate| {
            let common_suffix = candidate
                .as_ref()
                .iter()
                .rev()
                .zip(path.iter().rev())
                .take_while(|(x, y)| x == y)
                .count();
            let common_prefix = preferred_prefix.as_ref().map_or(0, |path| {
                candidate
                    .as_ref()
                    .iter()
                    .zip(path.as_ref().iter())
                    .take_while(|(x, y)| x == y)
                    .count()
            });
            (common_suffix, common_prefix, candidate)
        })
        .max()?;
    Some((path, candidate.2.as_ref()))
}

fn pick_candidates<'a>(
    candidates: HashMap<(&'a PathBuf, u64), &'a Vec<PathBuf>>,
) -> HashMap<&'a Path, &'a Path> {
    // Heuristic: If the file with the largest size has a single unique match, prefer matches that
    // share a common prefix.
    let largest_file_candidate_path = candidates
        .iter()
        .max_by_key(|((_path, len), _candidates)| len)
        .and_then(|(_, candidates)| {
            if candidates.len() == 1 {
                candidates.iter().next()
            } else {
                None
            }
        });
    // TODO: This doesn't prevent duplicate assignments, which is probably not desirable.
    candidates
        .into_iter()
        .map(|((path, _len), candidates)| {
            get_best_candidate(path, candidates, &largest_file_candidate_path).unwrap()
        })
        .collect()
}

fn process_torrent(
    path: &Path,
    target_dir: &Path,
    entries: &HashMap<u64, Vec<PathBuf>>,
    pieces_to_test: usize,
    dry_run: bool,
    skip_add: bool,
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
            Ok(((&file.path, file.length), entry))
        })
        .collect::<Result<HashMap<_, _>, _>>()?;
    let candidates = pick_candidates(candidates);
    let pieces = if dry_run || skip_add {
        // Sample a number of pieces to file as a quick correctness check.
        let mut path_to_pieces = HashMap::<_, Vec<_>>::new();
        for piece in &torrent.info.pieces {
            for slice in &piece.file_slices {
                path_to_pieces.entry(&slice.path).or_default().push(piece);
            }
        }
        std::borrow::Cow::Owned(
            path_to_pieces
                .into_iter()
                .flat_map(|(_path, mut pieces)| {
                    let piece_count = std::cmp::min(pieces_to_test, pieces.len());
                    pieces.shuffle(&mut rand::rng());
                    pieces.truncate(piece_count);
                    pieces
                })
                .collect::<HashSet<_>>()
                .into_iter()
                .cloned()
                .collect::<Vec<_>>(),
        )
    } else {
        // Otherwise, do a full check: the hash checks are parallelized and can run faster than
        // hash checks in many common torrent clients.
        std::borrow::Cow::Borrowed(&torrent.info.pieces)
    };
    let bar = util::new_bar(torrent.info.pieces.len() as u64).with_message("hashing...");
    let failed_paths: HashSet<_> = pieces
        .par_iter()
        .progress_with(bar)
        // TODO: Probably want some sort of error handling here.
        .filter(|piece| !piece.check(&candidates).unwrap())
        .flat_map_iter(|piece| piece.file_slices.iter().map(|slice| &slice.path))
        .collect();
    if !failed_paths.is_empty() {
        bail!("hash check failed for paths: {failed_paths:?}\n\ncandidates: {candidates:?}");
    }

    torrent.cross_seed(dry_run, skip_add, path, target_dir, &candidates)
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
            args.skip_add,
        ) {
            println!("{} {:?}", style("error:").red(), style(err).red());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_common_suffix_all_unique() {
        // Absolute
        assert_eq!(
            Path::new("/a/b/c").remove_common_suffix(Path::new("/d/e")),
            None
        );
        assert_eq!(
            Path::new("/a/b/c").remove_common_suffix(Path::new("/d/e/f")),
            None
        );
        assert_eq!(
            Path::new("/a/b/c").remove_common_suffix(Path::new("/d/e/f/g")),
            None
        );

        // Relative
        assert_eq!(
            Path::new("/a/b/c").remove_common_suffix(Path::new("d/e")),
            None
        );
        assert_eq!(
            Path::new("/a/b/c").remove_common_suffix(Path::new("d/e/f")),
            None
        );
        assert_eq!(
            Path::new("/a/b/c").remove_common_suffix(Path::new("d/e/f/g")),
            None
        );
    }

    #[test]
    fn remove_common_suffix_partial_shared() {
        // TODO: This doesn't seem quite right, but the logic seems to more or less work for now...
        assert_eq!(
            Path::new("/a/b/c").remove_common_suffix(Path::new("/b/c")),
            None,
        );

        assert_eq!(
            Path::new("/a/b/c").remove_common_suffix(Path::new("b/c")),
            Some("/a".into()),
        );
    }

    #[test]
    fn remove_common_suffix_all_shared() {
        assert_eq!(
            Path::new("/a/b/c").remove_common_suffix(Path::new("/a/b/c")),
            None,
        );

        assert_eq!(
            Path::new("/a/b/c").remove_common_suffix(Path::new("a/b/c")),
            Some("/".into()),
        );
    }

    #[test]
    fn get_best_candidate_single_option() {
        assert_eq!(
            get_best_candidate(Path::new("b/c"), &vec![Path::new("/a/b/c")], &None::<&Path>),
            Some((Path::new("b/c"), Path::new("/a/b/c")))
        );

        // With only a single option, `preferred_prefix` should have no effect on the result.
        assert_eq!(
            get_best_candidate(
                Path::new("b/c"),
                &vec![Path::new("/a/b/c")],
                &Some(Path::new("/a2/b2/c2"))
            ),
            Some((Path::new("b/c"), Path::new("/a/b/c")))
        );

        assert_eq!(
            get_best_candidate(
                Path::new("b/c"),
                &vec![Path::new("/a/b/c")],
                &Some(Path::new("/a/b/c"))
            ),
            Some((Path::new("b/c"), Path::new("/a/b/c")))
        );
    }

    #[test]
    fn get_best_candidate_preferred_prefix_disambiguates() {
        assert_eq!(
            get_best_candidate(
                Path::new("b/c"),
                &vec![Path::new("/a/b/c"), Path::new("/a2/b/c")],
                &Some(Path::new("/a"))
            ),
            Some((Path::new("b/c"), Path::new("/a/b/c")))
        );

        assert_eq!(
            get_best_candidate(
                Path::new("b/c"),
                &vec![Path::new("/a/b/c"), Path::new("/a2/b/c")],
                &Some(Path::new("/a/b"))
            ),
            Some((Path::new("b/c"), Path::new("/a/b/c")))
        );

        assert_eq!(
            get_best_candidate(
                Path::new("b/c"),
                &vec![Path::new("/a/b/c"), Path::new("/a2/b/c")],
                &Some(Path::new("/a/b2"))
            ),
            Some((Path::new("b/c"), Path::new("/a/b/c")))
        );
    }

    #[test]
    fn get_best_candidate_preferred_prefix_matches_nothing() {
        // The implementation takes the max tuple candidate; in this case, only the final path
        // component of the tuple will differ, so the implementation will return the "max" path.
        assert_eq!(
            get_best_candidate(
                Path::new("b/c"),
                &vec![Path::new("/a/b/c"), Path::new("/a2/b/c")],
                &Some(Path::new("/e"))
            ),
            Some((Path::new("b/c"), Path::new("/a2/b/c")))
        );
    }

    #[test]
    fn get_best_candidate_longest_shared_suffix_wins() {
        assert_eq!(
            get_best_candidate(
                Path::new("b/c"),
                &vec![Path::new("/a/b/c"), Path::new("/a/b2/c")],
                &None::<&Path>,
            ),
            Some((Path::new("b/c"), Path::new("/a/b/c")))
        );
    }

    #[test]
    fn get_best_candidate_shared_longest_shared_suffix() {
        // The implementation takes the max tuple candidate; in this case, only the final path
        // component of the tuple will differ, so the implementation will return the "max" path.
        assert_eq!(
            get_best_candidate(
                Path::new("b/c"),
                &vec![Path::new("/a/b/c"), Path::new("/a2/b/c")],
                &None::<&Path>,
            ),
            Some((Path::new("b/c"), Path::new("/a2/b/c")))
        );
    }

    #[test]
    fn get_best_candidate_prefer_suffix_over_prefix() {
        assert_eq!(
            get_best_candidate(
                Path::new("b/c"),
                &vec![Path::new("/a/b/c"), Path::new("/a/b2/c")],
                &Some(Path::new("/a/b2"))
            ),
            Some((Path::new("b/c"), Path::new("/a/b/c")))
        );
    }
}
