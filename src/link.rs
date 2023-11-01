use std::fs;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::borrow::Cow;
use std::collections::HashMap;
use anyhow::Context;
use argh::FromArgs;
use bstr::ByteSlice;
use object::{ Object, ObjectSection, ObjectSymbol };
use object::read::archive::ArchiveFile;
use object::read::File;
use memmap2::Mmap;
use crate::common::IteratorExt;


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

    loop {
        match rl.readline("explorer > ") {
            Ok(line) => if let Err(err) = exec(&mut explorer, &line) {
                eprintln!("exec failed: {:?}", err);
            },
            Err(ReadlineError::WindowResized) => (),
            Err(ReadlineError::Eof) | Err(ReadlineError::Interrupted) => break,
            Err(err) => return Err(err.into())
        }
    }

    Ok(())
}

fn exec<'buf>(explorer: &mut Explorer<'_, 'buf>, line: &str) -> anyhow::Result<()> {
    let mut iter = line.trim().split_whitespace();

    match iter.next() {
        Some("section") => for obj in explorer.list {
            println!("{:?}: {:#?}",
                obj.name.as_bstr(),
                obj.file.sections()
                    .map(|section| section.name().unwrap_or("<error>").to_string())
                    .filter(|name| !name.is_empty())
                    .collect::<Vec<_>>()
            );
        },
        Some("dump") => {
            let name = iter.next().context("need symbol name")?;
            let syms = explorer.search(name)?;

            assert!(!syms.is_empty());

            let pos = if syms.len() == 1 {
                syms[0].clone()
            } else if let Some(idx) = iter.next() {
                let idx: usize = idx.parse().context("need index number")?;
                syms.get(idx).context("index too large")?.clone()
            } else {
                for (idx, pos) in syms.iter()
                    .enumerate()
                    .filter(|(_, &pos)| explorer.index(pos).is_ok())
                {
                    let name = &explorer.list[pos.obj_idx].name;
                    eprintln!("[{}] {:?}", idx, name.as_bstr());
                }

                anyhow::bail!("duplicate symbol");
            };

            let sym = explorer.index(pos)?;
            let data = explorer.dump(&sym)?;

            let insns = explorer.disasm.disasm_all(data, sym.address)
                .map_err(|err| anyhow::format_err!("disasm failed: {:?}", err))?;

            for ins in insns.iter() {
                println!("{}", ins);
            }
        },
        _ => ()
    }

    Ok(())
}

struct Explorer<'a, 'buf> {
    list: &'a [ObjectFile<'buf>],
    symmap_list: Vec<object::read::SymbolMap<object::read::SymbolMapName<'buf>>>,
    arch: object::Architecture,
    format: object::BinaryFormat,
    disasm: capstone::Capstone,
    symbol_map: HashMap<&'buf str, Vec<SymbolPosition>>
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
    size: u64
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

        let symmap_list = if format == object::BinaryFormat::MachO {
            list.iter()
                .map(|obj| obj.file.symbol_map())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        let disasm = capstone::Capstone::new();
        let disasm = match arch {
            object::Architecture::Aarch64 => disasm
                .arm64()
                .mode(capstone::arch::arm64::ArchMode::Arm)
                .build(),
            object::Architecture::X86_64 => disasm
                .x86()
                .mode(capstone::arch::x86::ArchMode::Mode64)
                .build(),
            _ => anyhow::bail!("unsupport arch: {:?}", arch)
        };
        let disasm = disasm.map_err(|err| anyhow::format_err!("build capstone failed: {:?}", err))?;

        Ok(Explorer {
            list, symmap_list, arch, format, disasm,
            symbol_map: HashMap::default()
        })
    }

    fn search(&mut self, name: &str) -> anyhow::Result<Vec<SymbolPosition>> {
        if self.symbol_map.is_empty() {
            for (idx, obj) in self.list.iter().enumerate() {
                for sym in obj.file.symbols() {
                    let sym_name = match sym.name() {
                        Ok(name) => name,
                        Err(err) => anyhow::bail!("bad symbol name: {:?}", err)
                    };
                    let pos = SymbolPosition {
                        obj_idx: idx,
                        sym_idx: sym.index()
                    };
                    self.symbol_map.entry(sym_name)
                        .or_default()
                        .push(pos.clone());
                }
            }
        }

        if let Some(sym) = self.symbol_map.get(name) {
            Ok(sym.clone())
        } else {
            anyhow::bail!("not found symbol")
        }
    }

    fn index(&self, pos: SymbolPosition) -> anyhow::Result<Symbol> {
        let sym = self.list[pos.obj_idx].file.symbol_by_index(pos.sym_idx)?;

        let section_idx = match sym.section() {
            object::read::SymbolSection::Section(idx) => idx,
            object::read::SymbolSection::Undefined => anyhow::bail!("symbol is undefined"),
            section => anyhow::bail!("bad section: {:?}", section)
        };

        let size = if self.format == object::BinaryFormat::MachO {
            let symmap = &self.symmap_list[pos.obj_idx];
            let idx = match symmap.symbols()
                .binary_search_by_key(&sym.address(), |sym| sym.address())
            {
                Ok(idx) => idx,
                Err(idx) => idx.checked_sub(1).context("not found address by symbol map")?
            };
            match symmap.symbols().get(idx + 1) {
                Some(next_addr) => next_addr.address() - sym.address(),
                None => {
                    let obj = &self.list[pos.obj_idx];
                    let section = obj.file.section_by_index(section_idx)?;
                    section.address() + section.size() - sym.address()
                }
            }
        } else {
            sym.size()
        };

        Ok(Symbol {
            pos, section_idx,
            address: sym.address(),
            size
        })
    }

    fn dump(&self, sym: &Symbol) -> anyhow::Result<&'buf [u8]> {
        let section = self.list[sym.pos.obj_idx]
            .file
            .section_by_index(sym.section_idx)?;

        let data = section.data_range(sym.address, sym.size)?;
        let data = data.context("the section does not contain the given range")?;
        Ok(data)
    }
}
