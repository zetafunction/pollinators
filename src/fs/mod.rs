use console::Style;
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
        let cyan = Style::new().cyan();
        println!("creating directories at {}", cyan.apply_to(path.display()));
        Ok(())
    }

    fn symlink(&self, original: &Path, link: &Path) -> std::io::Result<()> {
        let cyan = Style::new().cyan();
        let magenta = Style::new().magenta();
        println!(
            "symlinking {} to {}",
            cyan.apply_to(link.display()),
            magenta.apply_to(original.display())
        );
        Ok(())
    }
}

pub fn get_dry_run_instance() -> Box<dyn Filesystem> {
    Box::new(DryRunFilesystem {})
}
