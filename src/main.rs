use std::{ fs, env };
use std::io::{ self, Write };
use bstr::ByteSlice;
use memmap::Mmap;
use object::Object;
use rustc_demangle::demangle;


fn main() -> anyhow::Result<()> {
    let mut args = env::args().skip(1);
    let fd = args.next()
        .ok_or_else(|| anyhow::format_err!("missing file path"))?;
    let search = args.next()
        .ok_or_else(|| anyhow::format_err!("missing search keyword"))?;

    let fd = fs::File::open(&fd)?;
    let mmap = unsafe { Mmap::map(&fd)? };
    let object = object::File::parse(mmap.as_ref())?;

    let mut count = 0;
    let mut namebuf = Vec::new();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    for symbol in object.symbol_map().symbols() {
        match symbol.kind() {
            object::SymbolKind::Section
                | object::SymbolKind::File
                => continue,
            _ => (),
        }

        if let Some(mangled_name) = symbol.name().filter(|name| !name.is_empty()) {
            write!(&mut namebuf, "{}", demangle(mangled_name))?;
            let name = namebuf.as_bytes();

            if name.contains_str(&search) || mangled_name.ends_with(&search) {
                let size = symbol.size();
                let addr = symbol.address();
                let kind = symbol.kind();

                count += size;

                writeln!(&mut stdout, "{:?}\t{:018p}\t{}\t\t{}", kind, addr as *const (), size, name.as_bstr())?;
            }

            namebuf.clear();
        }
    }

    writeln!(&mut stdout, "total:\t\t\t{}", count)?;

    Ok(())
}
