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
                    input.insert(format!("{:#}", demangle(name)).into_bytes());
                },
                None => ()
            }

            Ok(true)
        })?;

        let mut count = 0;
        let mut namebuf = Vec::new();

        let stdout = io::stdout();
        let mut stdout = stdout.lock();

        for symbol in oobj.symbol_map().symbols() {
            if symbol.kind() != object::SymbolKind::Text {
                continue
            }

            if let Some(mangled_name) = symbol.name().filter(|name| !name.is_empty()) {
                namebuf.clear();
                write!(&mut namebuf, "{:#}", demangle(mangled_name))?;
                let name = namebuf.as_bytes();

                if !input.contains(name) {
                    continue
                }

                let addr = symbol.address();
                let size = symbol.size();

                count += size;

                writeln!(&mut stdout, "{:018p}\t{}\t\t{}", addr as *const (), size, name.as_bstr())?;
            }
        }

        writeln!(&mut stdout, "total:\t\t\t{}", count)?;

        Ok(())
    }
}
