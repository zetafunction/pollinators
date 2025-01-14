use std::path::Path;

pub trait Filesystem {
    fn create_dir_all(&self, path: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(path)
    }

    fn symlink(&self, original: &Path, link: &Path) -> std::io::Result<()>;
}

struct PosixFilesystem;

impl Filesystem for PosixFilesystem {
    fn symlink(&self, original: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(original, link)
    }
}

pub fn get_default_instance() -> Box<dyn Filesystem> {
    Box::new(PosixFilesystem {})
}

struct DryRunFilesystem;

impl Filesystem for DryRunFilesystem {
    fn create_dir_all(&self, path: &Path) -> std::io::Result<()> {
        println!("creating directories at {}", path.display());
        Ok(())
    }

    fn symlink(&self, original: &Path, link: &Path) -> std::io::Result<()> {
        println!("symlinking {} to {}", link.display(), original.display());
        Ok(())
    }
}

pub fn get_dry_run_instance() -> Box<dyn Filesystem> {
    Box::new(DryRunFilesystem {})
}
