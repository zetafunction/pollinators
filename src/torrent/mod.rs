use serde::Deserialize;
use serde_bytes::ByteBuf;
use std::path::PathBuf;

#[derive(Debug, Eq, Hash, PartialEq)]
pub struct Digest([u8; sha1_smol::DIGEST_LENGTH]);

impl Digest {
    pub fn bytes(&self) -> [u8; sha1_smol::DIGEST_LENGTH] {
        self.0
    }
}

#[derive(Debug, Deserialize)]
pub struct File {
    pub length: u64,
    #[serde(deserialize_with = "deserialize_path_vec")]
    pub path: PathBuf,
}

fn deserialize_path_vec<'de, D>(deserializer: D) -> Result<PathBuf, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let path_pieces = <Vec<String>>::deserialize(deserializer)?;
    Ok(path_pieces.iter().collect())
}

#[derive(Debug, Eq, Hash, PartialEq)]
pub struct FileSlice {
    pub path: PathBuf,
    pub offset: u64,
    pub length: u64,
}

#[derive(Debug, Eq, Hash, PartialEq)]
pub struct Piece {
    pub hash: Digest,
    pub file_slices: Vec<FileSlice>,
}

#[derive(Debug)]
pub struct Info {
    // TODO: This should be an enum.
    // Only present for multi-file torrents.
    pub files: Option<Vec<File>>,
    // Only present for single-file torrents.
    pub length: Option<u64>,
    pub name: String,
    pub piece_length: u64,
    pub pieces: Vec<Piece>,
}

#[derive(Deserialize)]
pub struct Torrent {
    pub announce: String,
    #[serde(deserialize_with = "deserialize_info")]
    pub info: Info,
}

fn deserialize_info<'de, D>(deserializer: D) -> Result<Info, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct RawInfo {
        files: Option<Vec<File>>,
        length: Option<u64>,
        name: String,
        #[serde(rename = "piece length")]
        piece_length: u64,
        #[serde(rename = "pieces", deserialize_with = "deserialize_pieces")]
        hashes: Vec<Digest>,
    }

    fn deserialize_pieces<'de, D>(deserializer: D) -> Result<Vec<Digest>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = <ByteBuf>::deserialize(deserializer)?;
        let chunks = s
            .chunks(sha1_smol::DIGEST_LENGTH)
            .map(|chunk| {
                Ok(Digest(
                    <[u8; sha1_smol::DIGEST_LENGTH]>::try_from(chunk)
                        .map_err(serde::de::Error::custom)?,
                ))
            })
            .collect::<Result<Vec<Digest>, D::Error>>()?;
        match chunks.last() {
            None => Err(serde::de::Error::invalid_length(
                0,
                &"expected non-empty pieces",
            )),
            Some(chunk) => {
                if chunk.0.len() != sha1_smol::DIGEST_LENGTH {
                    Err(serde::de::Error::invalid_length(
                        s.len(),
                        &"pieces should be a multiple of 16",
                    ))
                } else {
                    Ok(chunks)
                }
            }
        }
    }

    let raw_info = RawInfo::deserialize(deserializer)?;

    let (is_single_file, files) = if let Some(files) = raw_info.files {
        (false, files)
    } else {
        (
            true,
            vec![File {
                length: raw_info.length.ok_or_else(|| {
                    serde::de::Error::custom("single-file torrent must have length set in info")
                })?,
                path: raw_info.name.clone().into(),
            }],
        )
    };
    let mut file_iter = files.iter().peekable();
    let mut remaining = files.iter().map(|f| f.length).sum();
    let mut file_remaining = files
        .first()
        .ok_or_else(|| serde::de::Error::custom("torrent with empty files in info"))?
        .length;
    let pieces = raw_info
        .hashes
        .into_iter()
        .map(|hash| {
            if remaining == 0 {
                return Err(serde::de::Error::custom(
                    "remaining hashes but all bytes consumed",
                ));
            }
            let mut piece_remaining = std::cmp::min(remaining, raw_info.piece_length);
            let mut file_slices = vec![];
            while piece_remaining > 0 {
                let current_file = file_iter.peek().ok_or_else(|| {
                    serde::de::Error::custom("remaining hashes but all files consumed")
                })?;
                let next = std::cmp::min(file_remaining, piece_remaining);
                file_slices.push(FileSlice {
                    path: current_file.path.clone(),
                    offset: current_file.length - file_remaining,
                    length: next,
                });
                if next >= file_remaining {
                    file_iter.next();
                    file_remaining = file_iter.peek().map(|file| file.length).unwrap_or(0);
                } else {
                    file_remaining -= next;
                }
                remaining -= next;
                piece_remaining -= next;
            }
            Ok(Piece { hash, file_slices })
        })
        .collect::<Result<_, D::Error>>()?;

    Ok(Info {
        files: if is_single_file { None } else { Some(files) },
        length: raw_info.length,
        name: raw_info.name,
        piece_length: raw_info.piece_length,
        pieces,
    })
}
