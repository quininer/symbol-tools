//! https://stffrdhrn.github.io/hardware/embedded/openrisc/2019/11/29/relocs.html

use std::fs;
use std::io::Write;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::borrow::Cow;
use std::collections::HashMap;
use aho_corasick::AhoCorasick;
use anyhow::Context;
use argh::FromArgs;
use bstr::ByteSlice;
use rustc_demangle::demangle;
use object::{ Object, ObjectSection, ObjectSymbol };
use object::read::archive::ArchiveFile;
use object::read::File;
use memmap2::Mmap;
use indexmap::IndexMap;
use crate::common::{ IteratorExt, DoubleLife, data_range, print_pretty_bytes };


/// Cross-platform Symbol Explorer
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "link")]
pub struct Options {
    /// object files
    #[argh(positional)]
    objects: Vec<PathBuf>,
}

impl Options {
    pub fn exec(self) -> anyhow::Result<()> {
        let fds = self.objects.iter()
            .map(|path| fs::File::open(path))
            .collect::<Result<Vec<_>, _>>()?;
        let bufs = fds.iter()
            .map(|fd| unsafe { Mmap::map(fd) })
            .collect::<Result<Vec<_>, _>>()?;
        let list = self.objects.iter()
            .zip(bufs.iter())
            .map(|(path, buf)| match path.extension() {
                Some(ext) if ext == OsStr::new("a") || ext == OsStr::new("rlib") =>
                    ArchiveFile::parse(buf.as_ref())
                        .map(|ar| Archive::Ar(buf, ar))
                        .map_err(Into::into),
                Some(ext) if ext == OsStr::new("o") => {
                    let name = path.file_name().context("bad file name")?;
                    // TODO use as_os_str_bytes
                    let name = name.to_str().context("not utf8 name")?;
                    let name = Cow::Owned(name.as_bytes().into());
                    File::parse(buf.as_ref())
                        .map(|file| Archive::Object(ObjectFile { name, file }) )
                        .map_err(Into::into)
                },
                ext => anyhow::bail!("no support extension: {:?}", ext)
            })
            .flat_result()
            .collect::<Result<Vec<_>, anyhow::Error>>()?;

        explorer(&list)
    }
}

struct ObjectFile<'buf> {
    name: Cow<'buf, [u8]>,
    file: File<'buf>
}

enum Archive<'buf> {
    Ar(&'buf [u8], ArchiveFile<'buf>),
    Object(ObjectFile<'buf>)
}

enum ArchiveIter<'buf> {
    Ar(&'buf [u8], object::read::archive::ArchiveMemberIterator<'buf>),
    Object(Option<ObjectFile<'buf>>)
}

impl<'buf> IntoIterator for Archive<'buf> {
    type Item = anyhow::Result<ObjectFile<'buf>>;
    type IntoIter = ArchiveIter<'buf>;

    fn into_iter(self) -> Self::IntoIter {
        match self {
            Archive::Ar(buf, ar) => ArchiveIter::Ar(buf, ar.members()),
            Archive::Object(obj) => ArchiveIter::Object(Some(obj))
        }
    }
}

impl<'buf> Iterator for ArchiveIter<'buf> {
    type Item = anyhow::Result<ObjectFile<'buf>>;

    fn next(&mut self) -> Option<Self::Item> {
        macro_rules! try_ {
            ( $e:expr ) => {
                match $e {
                    Ok(e) => e,
                    Err(err) => return Some(Err(err.into()))
                }
            }
        }

        match self {
            ArchiveIter::Ar(buf, iter) => {
                let member = try_!(iter.next()?);
                let name = Cow::Borrowed(member.name());
                let data = try_!(member.data(*buf));
                let file =  try_!(File::parse(data));
                Some(Ok(ObjectFile { name, file }))
            },
            ArchiveIter::Object(obj) => obj.take().map(Ok)
        }
    }
}

fn explorer(list: &[ObjectFile<'_>]) -> anyhow::Result<()> {
    use rustyline::error::ReadlineError;

    let config = rustyline::config::Builder::new()
        .completion_type(rustyline::config::CompletionType::List)
        .edit_mode(rustyline::config::EditMode::Vi)
        .auto_add_history(true)
        .build();
    let mut rl = rustyline::DefaultEditor::with_config(config)?;

    let mut explorer = Explorer::build(list)?;
    let mut cache = Cache::default();

    loop {
        match rl.readline("explorer > ") {
            Ok(line) => if let Err(err) = exec(&mut explorer, &mut cache, &line) {
                eprintln!("failed: {:?}", err);
            },
            Err(ReadlineError::WindowResized) => (),
            Err(ReadlineError::Eof) | Err(ReadlineError::Interrupted) => break,
            Err(err) => return Err(err.into())
        }
    }

    Ok(())
}

fn exec<'buf>(explorer: &mut Explorer<'_, 'buf>, cache: &mut Cache<'buf>, line: &str)
    -> anyhow::Result<()>
{
    let mut iter = line.trim().split_whitespace();

    match iter.next() {
        Some("obj") => match iter.next() {
            Some("none") => explorer.current_obj_idx = None,
            Some(name) => {
                let obj_idx = explorer.list.iter()
                    .enumerate()
                    .find(|(_, obj)| obj.name == name.as_bytes())
                    .map(|(idx, _)| idx)
                    .context("not found object")?;
                explorer.current_obj_idx = Some(obj_idx);
            }
            None => match explorer.current_obj_idx {
                Some(obj_idx) => println!("{}", explorer.list[obj_idx].name.as_bstr()),
                None => println!("none")
            }
        },
        Some("section") => for obj in explorer.list {
            println!("{:?}: {:#?}",
                obj.name.as_bstr(),
                obj.file.sections()
                    .map(|section| section.name().unwrap_or("<error>").to_string())
                    .filter(|name| !name.is_empty())
                    .collect::<Vec<_>>()
            );
        },
        Some("search") => {
            let keywords = iter.collect::<Vec<_>>();
            if keywords.is_empty() {
                anyhow::bail!("need keyword");
            }
            let ac = AhoCorasick::new(&keywords)?;

            explorer.symbol_map.iter().fast_for_each(|(mangled_name, syms)| -> anyhow::Result<()> {
                use smallvec::SmallVec;

                let mut namebuf = SmallVec::<[u8; 1024 * 4]>::new();
                write!(&mut namebuf, "{}", demangle(mangled_name))?;
                let name = namebuf.as_slice();

                if ac.is_match(&name) || keywords.iter().any(|w| mangled_name.ends_with(w)) {
                    for &pos in syms {
                        let obj = &explorer.list[pos.obj_idx];
                        let sym = obj.file.symbol_by_index(pos.sym_idx)?;
                        let kind = explorer.symbol_kind(pos);

                        println!("{:016x} {} {} @ {:?}",
                            sym.address(),
                            kind,
                            mangled_name,
                            obj.name.as_bstr(),
                        );
                    }
                }

                Ok(())
            })?;
        },
        Some("dump") => {
            let name = iter.next().context("need symbol name")?;
            let syms = explorer.get(name)?;

            let pos = match select(explorer, syms, iter.next())? {
                Some(pos) => pos,
                None => {
                    print_syms_list(explorer, &syms)?;
                    anyhow::bail!("duplicate symbol");
                }
            };

            let sym = explorer.index(cache, pos)?;
            let data = explorer.dump(cache, &sym)?;
            let data = data.as_ref();
            let obj = &explorer.list[sym.pos.obj_idx];
            let section = obj.file.section_by_index(sym.section_idx)?;
            let address = sym.address - section.address();

            println!("{:016x} {} {} @ {}/{}",
                sym.address,
                sym.kind,
                sym.size,
                obj.name.as_bstr(),
                section.name()?
            );

            if matches!(sym.kind, 't' | 'T') {
                let disasm = (explorer.disasm)()?;
                let insns = disasm.disasm_all(data, address)
                    .map_err(|err| anyhow::format_err!("disasm failed: {:?}", err))?;

                for ins in insns.iter() {
                    println!("{}", ins);
                }
            } else {
                let stdout = std::io::stdout();
                let mut stdout = stdout.lock();

                print_pretty_bytes(&mut stdout, address, data)?;
            }
        },
        Some("reloc") => {
            let name = iter.next().context("need symbol name")?;
            let syms = explorer.get(name)?;

            let pos = match select(explorer, syms, iter.next())? {
                Some(pos) => pos,
                None => {
                    print_syms_list(explorer, &syms)?;
                    anyhow::bail!("duplicate symbol");
                }
            };

            let sym = explorer.index(cache, pos)?;
            let list = explorer.reloc(cache, &sym)?;
            let obj = &explorer.list[sym.pos.obj_idx];

            let stdout = std::io::stdout();
            let mut stdout = stdout.lock();

            if !list.is_empty() {
                writeln!(stdout, "OFFSET           ADDEND               TYPE    ADDRESS          NAME")?;
            }

            for reloc in &list {
                let (ty, addr, name) = match &reloc.target {
                    RelocationTarget::Symbol(idx) => {
                        let sym = obj.file.symbol_by_index(*idx)?;
                        let addr = sym.address();
                        let name = sym.name()?.to_string();
                        ("symbol", addr, name)
                    },
                    RelocationTarget::Section(idx) => {
                        let section = obj.file.section_by_index(*idx)?;
                        let addr = section.address();
                        let name = section.name()?.to_string();
                        ("section", addr, name)
                    }
                };

                writeln!(stdout,
                    "{:016x} {:<20} {:<7} {:016x} {}",
                    reloc.offset,
                    reloc.addend,
                    ty,
                    addr,
                    name
                )?;
            }
        },
        Some(cmd) if !cmd.trim().is_empty() => anyhow::bail!("unknown command"),
        _ => ()
    }

    Ok(())
}

struct Explorer<'a, 'buf> {
    list: &'a [ObjectFile<'buf>],
    #[allow(dead_code)] arch: object::Architecture,
    format: object::BinaryFormat,
    disasm: fn() -> anyhow::Result<capstone::Capstone>,
    symbol_map: IndexMap<&'buf str, Vec<SymbolPosition>>,
    current_obj_idx: Option<usize>
}

#[derive(Default)]
struct Cache<'buf> {
    symmap_list: Vec<object::read::SymbolMap<object::read::SymbolMapName<'buf>>>,
    decompress_sections: HashMap<(usize, object::read::SectionIndex), (u64, Vec<u8>)>,
    reloc_list: Vec<HashMap<object::read::SectionIndex, Vec<(u64, object::read::Relocation)>>>
}

#[derive(Clone, Copy)]
struct SymbolPosition {
    obj_idx: usize,
    sym_idx: object::read::SymbolIndex
}

struct Symbol {
    pos: SymbolPosition,
    section_idx: object::read::SectionIndex,
    address: u64,
    size: u64,
    kind: char
}

#[derive(Debug)]
struct Relocation {
    offset: u64,
    target: RelocationTarget,
    addend: i64
}

#[derive(Debug)]
enum RelocationTarget {
    Symbol(object::read::SymbolIndex),
    Section(object::read::SectionIndex)
}

impl<'a, 'buf> Explorer<'a, 'buf> {
    fn build(list: &'a [ObjectFile<'buf>]) -> anyhow::Result<Explorer<'a, 'buf>> {
        use capstone::arch::BuildsCapstone;

        let (arch, format) = {
            let obj = list.iter().next().context("not found object")?;
            let arch = obj.file.architecture();
            let format = obj.file.format();

            if let Some(obj) = list.iter().find(|obj| obj.file.architecture() != arch) {
                anyhow::bail!("inconsistent architecture: {:?} vs {:?} - {}",
                    arch,
                    obj.file.architecture(),
                    obj.name.as_bstr()
                );
            }

            if let Some(obj) = list.iter().find(|obj| obj.file.format() != format) {
                anyhow::bail!("inconsistent format: {:?} vs {:?} - {}",
                    format,
                    obj.file.format(),
                    obj.name.as_bstr()
                );
            }

            (arch, format)
        };

        let disasm = match arch {
            object::Architecture::Aarch64 => || {
                capstone::Capstone::new()
                    .arm64()
                    .mode(capstone::arch::arm64::ArchMode::Arm)
                    .build()
                    .map_err(|err| anyhow::format_err!("build capstone failed: {:?}", err))
            },
            object::Architecture::X86_64 => || {
                capstone::Capstone::new()
                    .x86()
                    .mode(capstone::arch::x86::ArchMode::Mode64)
                    .build()
                    .map_err(|err| anyhow::format_err!("build capstone failed: {:?}", err))
            },
            _ => anyhow::bail!("unsupport arch: {:?}", arch)
        };

        let mut symbol_map: IndexMap<_, Vec<SymbolPosition>> = IndexMap::new();
        for (idx, obj) in list.iter().enumerate() {
            for sym in obj.file.symbols() {
                let sym_name = match sym.name() {
                    Ok(name) => name,
                    Err(err) => anyhow::bail!("bad symbol name: {:?}", err)
                };
                let pos = SymbolPosition {
                    obj_idx: idx,
                    sym_idx: sym.index()
                };
                symbol_map.entry(sym_name)
                    .or_default()
                    .push(pos);
            }
        }
        symbol_map.values_mut().for_each(|list| list.shrink_to_fit());
        symbol_map.shrink_to_fit();

        Ok(Explorer {
            list, arch, format, disasm,
            symbol_map,
            current_obj_idx: None
        })
    }

    fn get<'list>(&'list self, name: &str) -> anyhow::Result<&'list [SymbolPosition]> {
        self.symbol_map.get(name)
            .map(|syms| syms.as_slice())
            .context("not found symbol")
    }

    fn index(&self, cache: &mut Cache<'buf>, pos: SymbolPosition) -> anyhow::Result<Symbol> {
        let sym = self.list[pos.obj_idx].file.symbol_by_index(pos.sym_idx)?;

        let section_idx = match sym.section() {
            object::read::SymbolSection::Section(idx) => idx,
            object::read::SymbolSection::Undefined => anyhow::bail!("symbol is undefined"),
            section => anyhow::bail!("bad section: {:?}", section)
        };

        let size = if self.format != object::BinaryFormat::MachO {
            sym.size()
        } else {
            cache.init_symmap(self)?;

            let symmap = &cache.symmap_list[pos.obj_idx];
            let idx = match symmap.symbols()
                .binary_search_by_key(&sym.address(), |sym| sym.address())
            {
                Ok(idx) => idx,
                Err(_) => anyhow::bail!("not found symbol address")
            };
            match symmap.symbols().get(idx + 1) {
                Some(next_addr) => next_addr.address() - sym.address(),
                None => {
                    let obj = &self.list[pos.obj_idx];
                    let section = obj.file.section_by_index(section_idx)?;
                    section.address() + section.size() - sym.address()
                }
            }
        };

        let kind = self.symbol_kind(pos);

        Ok(Symbol {
            pos, section_idx,
            address: sym.address(),
            size, kind
        })
    }

    fn dump<'cache>(&self, cache: &'cache mut Cache<'buf>, sym: &Symbol)
        -> anyhow::Result<DoubleLife<'cache, 'buf, [u8]>>
    {
        use std::collections::hash_map::Entry;

        let cache_idx = (sym.pos.obj_idx, sym.section_idx);

        match cache.decompress_sections.entry(cache_idx) {
            Entry::Occupied(entry) => {
                let (section_addr, data) = entry.into_mut();
                data_range(data, *section_addr, sym.address, sym.size)
                    .map(DoubleLife::Left)
            },
            Entry::Vacant(entry) => {
                let obj = &self.list[sym.pos.obj_idx];
                let section = obj.file.section_by_index(sym.section_idx)?;
                let section_addr = section.address();

                match section.uncompressed_data()? {
                    Cow::Borrowed(data) => data_range(data, section_addr, sym.address, sym.size)
                        .map(DoubleLife::Right),
                    Cow::Owned(data) => {
                        let data = entry.insert((section_addr, data))
                            .1
                            .as_slice();
                        data_range(data, section_addr, sym.address, sym.size)
                            .map(DoubleLife::Left)
                    }
                }
            }
        }
    }

    fn reloc<'cache>(&self, cache: &'cache mut Cache<'buf>, sym: &Symbol)
        -> anyhow::Result<Vec<Relocation>>
    {
        cache.init_reloc(self, &sym)?;

        let relocs = cache.reloc_list[sym.pos.obj_idx]
            .get(&sym.section_idx)
            .unwrap();
        let section = self.list[sym.pos.obj_idx].file.section_by_index(sym.section_idx)?;

        let address = sym.address - section.address();
        let start = relocs.partition_point(|(offset, _)| *offset < address);
        let end = relocs.partition_point(|(offset, _)| *offset < address + sym.size);

        let mut list = Vec::new();
        for (offset, reloc) in relocs.get(start..end).unwrap_or_default() {
            list.push(Relocation {
                offset: *offset,
                target: match reloc.target() {
                    object::read::RelocationTarget::Symbol(idx) => RelocationTarget::Symbol(idx),
                    object::read::RelocationTarget::Section(idx) => RelocationTarget::Section(idx),
                    _ => anyhow::bail!("not support target: {:?}", reloc)
                },
                addend: reloc.addend()
            });
        }

        Ok(list)
    }

    fn symbol_kind(&self, pos: SymbolPosition) -> char {
        use object::{ SymbolSection, SectionKind };

        let obj = &self.list[pos.obj_idx];
        let sym = obj.file.symbol_by_index(pos.sym_idx).unwrap();

        let mut kind = match sym.section() {
            SymbolSection::Undefined => 'U',
            SymbolSection::Absolute => 'A',
            SymbolSection::Common => 'C',
            SymbolSection::Section(idx) => match obj.file.section_by_index(idx).map(|section| section.kind()) {
                Ok(SectionKind::Text) => 't',
                Ok(SectionKind::Data) | Ok(SectionKind::Tls) | Ok(SectionKind::TlsVariables) => {
                    'd'
                }
                Ok(SectionKind::ReadOnlyData) | Ok(SectionKind::ReadOnlyString) => 'r',
                Ok(SectionKind::UninitializedData) | Ok(SectionKind::UninitializedTls) => 'b',
                Ok(SectionKind::Common) => 'C',
                _ => '?',
            },
            _ => '?',
        };

        if sym.is_global() {
            kind = kind.to_ascii_uppercase();
        }

        kind
    }
}

impl<'buf> Cache<'buf> {
    fn init_symmap(&mut self, explorer: &Explorer<'_, 'buf>) -> anyhow::Result<()> {
        if explorer.format != object::BinaryFormat::MachO || !self.symmap_list.is_empty() {
            return Ok(())
        }

        self.symmap_list = explorer.list.iter()
            .map(|obj| obj.file.symbol_map())
            .collect::<Vec<_>>();

        Ok(())
    }

    fn init_reloc(&mut self, explorer: &Explorer<'_, 'buf>, sym: &Symbol)
        -> anyhow::Result<()>
    {
        if self.reloc_list.is_empty() {
            self.reloc_list = explorer
                .list
                .iter()
                .map(|_| HashMap::new())
                .collect::<Vec<_>>()
        }

        let obj = &explorer.list[sym.pos.obj_idx];

        let map = &mut self.reloc_list[sym.pos.obj_idx];
        let list = map.entry(sym.section_idx).or_default();

        if list.is_empty() {
            let section = obj.file.section_by_index(sym.section_idx)?;

            for (offset, reloc) in section.relocations() {
                list.push((offset, reloc));
            }

            list.sort_by_key(|(offset, _)| *offset);
            list.shrink_to_fit();
        }

        Ok(())
    }
}

fn print_syms_list(
    explorer: &Explorer<'_, '_>,
    syms: &[SymbolPosition]
) -> anyhow::Result<()> {
    for (idx, &pos) in syms.iter()
        .enumerate()
    {
        let name = &explorer.list[pos.obj_idx].name;
        let kind = explorer.symbol_kind(pos);
        eprintln!("[{}] {} by {:?}", idx, kind, name.as_bstr());
    }

    Ok(())
}

fn select(explorer: &Explorer<'_, '_>, syms: &[SymbolPosition], iter: Option<&str>)
    -> anyhow::Result<Option<SymbolPosition>>
{
    assert!(!syms.is_empty());

    let syms = if syms.len() == 1 {
        Cow::Borrowed(syms)
    } else if let Some(obj_idx) = explorer.current_obj_idx {
        let syms = syms.iter()
            .filter(|sym| sym.obj_idx == obj_idx)
            .copied()
            .collect::<Vec<_>>();
        Cow::Owned(syms)
    } else {
        Cow::Borrowed(syms)
    };

    Ok(if syms.len() == 1 {
        Some(syms[0])
    } else if let Some(idx) = iter {
        let idx: usize = idx.parse().context("need index number")?;
        let pos = syms.get(idx).copied().context("index too large")?;
        Some(pos)
    } else {
        None
    })
}
