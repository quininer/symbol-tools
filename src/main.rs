use std::{ fs, cmp };
use std::io::{ self, Write };
use std::path::PathBuf;
use std::collections::BTreeMap;
use aho_corasick::AhoCorasick;
use bstr::ByteSlice;
use memmap::Mmap;
use object::Object;
use object::read::Symbol;
use rustc_demangle::demangle;
use argh::FromArgs;


/// Cross-platform Symbol Searcher
#[derive(FromArgs, PartialEq, Debug)]
struct Options {
    /// object file
    #[argh(positional)]
    file: PathBuf,

    /// search keywords
    #[argh(positional)]
    keywords: Vec<String>,

    /// sort by size
    #[argh(switch)]
    sort: bool
}

struct Filter<'a, 'data> {
    object: object::File<'data>,
    keywords: &'a [String]
}

struct SizeAddr(u64, u64);

impl<'a, 'data> Filter<'a, 'data> {
    fn new(obj: object::File<'data>, keywords: &'a [String]) -> Filter<'a, 'data> {
        Filter {
            object: obj,
            keywords
        }
    }

    fn for_each<F>(&self, mut f: F) -> anyhow::Result<()>
    where
        F: FnMut(&[u8], &Symbol<'data>) -> anyhow::Result<()>
    {
        let ac = AhoCorasick::new(self.keywords);
        let mut namebuf = Vec::new();

        for symbol in self.object.symbol_map().symbols() {
            if let Some(mangled_name) = symbol.name().filter(|name| !name.is_empty()) {
                write!(&mut namebuf, "{}", demangle(mangled_name))?;
                let name = namebuf.as_bytes();

                if ac.is_match(&name) || self.keywords.iter().any(|w| mangled_name.ends_with(w)) {
                    f(name, symbol)?;
                }

                namebuf.clear();
            }
        }

        Ok(())
    }
}


fn main() -> anyhow::Result<()> {
    let Options { file, keywords, sort } = argh::from_env();

    let fd = fs::File::open(&file)?;

    if keywords.is_empty() {
        return Err(anyhow::format_err!("search keyword is empty"));
    }

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
            let kind = symbol.kind();
            let addr = symbol.address();

            count += size;

            writeln!(&mut stdout, "{:?}\t{:018p}\t{}\t\t{}", kind, addr as *const (), size, name.as_bstr())?;

            Ok(())
        })?;
    } else {
        let mut symbolset = BTreeMap::new();

        filter.for_each(|name, symbol| {
            symbolset.insert(
                SizeAddr(symbol.size(), symbol.address()),
                (symbol.kind(), Vec::from(name))
            );

            Ok(())
        })?;

        for (SizeAddr(size, addr), (kind, name)) in symbolset {
            count += size;

            writeln!(&mut stdout, "{:?}\t{:018p}\t{}\t\t{}", kind, addr as *const (), size, name.as_bstr())?;
        }
    }

    writeln!(&mut stdout, "total:\t\t\t{}", count)?;

    Ok(())
}

impl PartialOrd for SizeAddr {
    #[inline]
    fn partial_cmp(&self, rhs: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(rhs))
    }
}

impl Ord for SizeAddr {
    #[inline]
    fn cmp(&self, rhs: &Self) -> cmp::Ordering {
        match self.0.cmp(&rhs.0) {
            cmp::Ordering::Equal => self.1.cmp(&rhs.1),
            ret => ret
        }
    }
}

impl PartialEq for SizeAddr {
    #[inline]
    fn eq(&self, rhs: &Self) -> bool {
        self.1 == rhs.1
    }
}

impl Eq for SizeAddr {}
