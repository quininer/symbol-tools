use std::fs;
use std::path::PathBuf;
use std::collections::BTreeSet;
use std::io::{ self, Write, BufReader };
use bstr::ByteSlice;
use bstr::io::BufReadExt;
use memmap::Mmap;
use object::Object;
use rustc_demangle::demangle;
use argh::FromArgs;
use crate::common::collect_map;


/// Cross-platform Symbol Finder
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "contains")]
pub struct Options {
    /// archive file
    #[argh(positional)]
    ar: PathBuf,

    /// object file
    #[argh(positional)]
    obj: PathBuf,
}

impl Options {
    pub fn exec(self) -> anyhow::Result<()> {
        let afd = fs::File::open(&self.ar)?;
        let ofd = fs::File::open(&self.obj)?;

        let areader = BufReader::new(afd);
        let omap = unsafe { Mmap::map(&ofd)? };
        let oobj = object::File::parse(omap.as_ref())?;

        if !oobj.has_debug_symbols() {
            eprintln!("WARN: The new file is missing debug symbols.");
        }

        let obj_map = collect_map(oobj.symbol_map().symbols());

        let mut input = BTreeSet::new();

        // llvm-nm -f bsd ./<your ar>
        areader.for_byte_line(|line| {
            let line = line.trim();

            if line.is_empty() || line.starts_with_str("../") {
                return Ok(true);
            }

            let mut words = line.words();
            let _ = words.next(); // ignore address

            let kind = words.next(); // text kind
            match kind {
                Some("t") | Some("T") => (),
                _ => return Ok(true)
            }

            match words.next() { // symbol name
                Some(name) => {
                    input.insert(format!("{:#}", demangle(name)));
                },
                None => ()
            }

            Ok(true)
        })?;

        let mut count = 0;

        let stdout = io::stdout();
        let mut stdout = stdout.lock();

        for name in input {
            if let Some(&(addr, size)) = obj_map.get(name.as_bytes()) {
                count += size;

                writeln!(&mut stdout, "{:018p}\t{}\t\t{}", addr as *const (), size, name)?;
            }
        }

        writeln!(&mut stdout, "total:\t\t\t{}", count)?;

        Ok(())
    }
}
