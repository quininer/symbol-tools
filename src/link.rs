use std::fs;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::borrow::Cow;
use object::read::archive::ArchiveFile;
use object::read::File;
use memmap2::Mmap;
use argh::FromArgs;


/// Cross-platform Symbol Explorer
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "search")]
pub struct Options {
    /// object files
    #[argh(positional)]
    objects: Vec<PathBuf>,
}

impl Options {
    pub fn exec(self) -> anyhow::Result<()> {
        let Options { objects } = self;

        let fds = objects.iter()
            .map(|path| fs::File::open(path))
            .collect::<Result<Vec<_>, _>>()?;
        let bufs = fds.iter()
            .map(|fd| unsafe { Mmap::map(fd) })
            .collect::<Result<Vec<_>, _>>()?;
        let objects = objects.iter()
            .zip(bufs.iter())
            .map(|(path, buf)| match path.extension() {
                Some(ext) if ext == OsStr::new("a")
                    => ArchiveFile::parse(buf.as_ref()).map(Archive::Ar).map_err(Into::into),
                Some(ext) if ext == OsStr::new("o")
                    => File::parse(buf.as_ref()).map(Archive::Object).map_err(Into::into),
                ext => anyhow::bail!("no support extension: {:?}", ext)
            })
            .collect::<Result<Vec<_>, anyhow::Error>>()?;

        //

        Ok(())
    }
}

enum Archive<'buf> {
    Ar(ArchiveFile<'buf>),
    Object(File<'buf>)
}
