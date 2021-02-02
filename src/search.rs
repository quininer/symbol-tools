use std::fs;
use std::io::{ self, Write };
use std::path::PathBuf;
use anyhow::Context;
use aho_corasick::AhoCorasick;
use bstr::ByteSlice;
use memmap::Mmap;
use object::{ Object, Symbol, SymbolKind, ObjectSymbolTable, ObjectSymbol };
use rustc_demangle::demangle;
use argh::FromArgs;


/// Cross-platform Symbol Searcher
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "search")]
pub struct Options {
    /// object file
    #[argh(positional)]
    file: PathBuf,

    /// search keywords
    #[argh(positional)]
    keywords: Vec<String>,

    /// sort by size
    #[argh(switch)]
    sort: bool,
}

struct Filter<'a, 'data> {
    object: object::File<'data>,
    keywords: &'a [String]
}

impl<'a, 'data> Filter<'a, 'data> {
    fn new(obj: object::File<'data>, keywords: &'a [String]) -> Filter<'a, 'data> {
        Filter {
            object: obj,
            keywords
        }
    }

    fn for_each<F>(&self, mut f: F) -> anyhow::Result<()>
    where
        F: FnMut(&[u8], Symbol) -> anyhow::Result<()>
    {
        let ac = if self.keywords.is_empty() {
            None
        } else {
            Some(AhoCorasick::new(self.keywords))
        };
        let mut namebuf = Vec::new();

        let symbol_table = self.object.symbol_table()
            .context("not found symbol_table")?;

        for symbol in symbol_table.symbols() {
            if symbol.kind() != SymbolKind::Text {
                continue
            }

            if let Some(mangled_name) = symbol.name().ok().filter(|name| !name.is_empty()) {
                write!(&mut namebuf, "{}", demangle(mangled_name))?;
                let name = namebuf.as_bytes();

                if ac.as_ref()
                    .map(|ac| ac.is_match(&name))
                    .unwrap_or(true)
                    || self.keywords.iter().any(|w| mangled_name.ends_with(w))
                {
                    f(name, symbol)?;
                }

                namebuf.clear();
            }
        }

        Ok(())
    }
}

impl Options {
    pub fn exec(self) -> anyhow::Result<()> {
        let Options { file, keywords, sort } = self;

        let fd = fs::File::open(&file)?;

        let mmap = unsafe { Mmap::map(&fd)? };
        let object = object::File::parse(mmap.as_ref())?;

        if !object.has_debug_symbols() {
            eprintln!("WARN: The file is missing debug symbols.");
        }

        let filter = Filter::new(object, &keywords);

        let mut count = 0;
        let stdout = io::stdout();
        let mut stdout = stdout.lock();

        if !sort {
            filter.for_each(|name, symbol| {
                let size = symbol.size();
                let addr = symbol.address();

                count += size;

                writeln!(&mut stdout, "{:018p}\t{}\t\t{}", addr as *const (), size, name.as_bstr())?;

                Ok(())
            })?;
        } else {
            let mut output = Vec::new();

            filter.for_each(|name, symbol| {
                output.push((symbol.address(), symbol.size(), Vec::from(name)));

                Ok(())
            })?;

            output.sort_unstable_by_key(|symbol| symbol.1);

            for (addr, size, name) in output {
                count += size;

                writeln!(&mut stdout, "{:018p}\t{}\t\t{}", addr as *const (), size, name.as_bstr())?;
            }
        }

        writeln!(&mut stdout, "total:\t\t\t{}", count)?;

        Ok(())
    }
}
