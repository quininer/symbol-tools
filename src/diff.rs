use std::fs;
use std::rc::Rc;
use std::path::PathBuf;
use std::collections::HashMap;
use std::io::{ self, Write };
use memmap::Mmap;
use object::Object;
use bstr::ByteSlice;
use argh::FromArgs;
use crate::common::collect_map;


/// Cross-platform Symbol Differ
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "diff")]
pub struct Options {
    /// old object file
    #[argh(positional)]
    old: PathBuf,

    /// new object file
    #[argh(positional)]
    new: PathBuf,

    /// oneway diff
    #[argh(switch)]
    oneway: bool,

    /// sort by size
    #[argh(switch)]
    sort: bool
}

pub struct Differ<'a>(&'a HashMap<Rc<[u8]>, (u64, u64)>, &'a HashMap<Rc<[u8]>, (u64, u64)>, bool);

impl Differ<'_> {
    fn for_each<F>(&self, mut f: F) -> anyhow::Result<()>
    where
        F: FnMut(&Rc<[u8]>, (u64, i64), (u64, i64)) -> anyhow::Result<()>
    {
        for (name, &(addr, size)) in self.0.iter() {
            if let Some(&(new_addr, new_size)) = self.1.get(name) {
                if size != new_size {
                    f(name, (addr, size as i64), (new_addr, new_size as i64))?;
                }
            } else {
                f(name, (addr, size as i64), (0, 0))?;
            }
        }

        if self.2 {
            for (name, &(new_addr, new_size)) in self.1.iter() {
                if !self.0.contains_key(name) {
                    f(name, (0, 0), (new_addr, new_size as i64))?;
                }
            }
        }

        Ok(())
    }
}

impl Options {
    pub fn exec(self) -> anyhow::Result<()> {
        let old_fd = fs::File::open(&self.old)?;
        let new_fd = fs::File::open(&self.new)?;

        let old_mmap = unsafe { Mmap::map(&old_fd)? };
        let old_obj = object::File::parse(old_mmap.as_ref())?;
        let new_mmap = unsafe { Mmap::map(&new_fd)? };
        let new_obj = object::File::parse(new_mmap.as_ref())?;

        if !old_obj.has_debug_symbols() {
            eprintln!("WARN: The old file is missing debug symbols.");
        }

        if !new_obj.has_debug_symbols() {
            eprintln!("WARN: The new file is missing debug symbols.");
        }

        let old_map = collect_map(old_obj.symbol_map().symbols());
        let new_map = collect_map(new_obj.symbol_map().symbols());

        let stdout = io::stdout();
        let mut stdout = stdout.lock();

        let mut change_count = 0;

        if !self.sort {
            Differ(&old_map, &new_map, !self.oneway)
                .for_each(|name, (old_addr, old_size), (new_addr, new_size)| {
                    let diff_size = new_size - old_size;

                    change_count += diff_size;

                    writeln!(&mut stdout,
                        "{:018p}\t{:018p}\t{}\t{}\t{}\t\t{}",
                        old_addr as *const (),
                        new_addr as *const (),
                        old_size,
                        new_size,
                        diff_size,
                        name.as_bstr()
                    )?;

                    Ok(())
                })?;
        } else {
            let mut output = Vec::new();

            Differ(&old_map, &new_map, !self.oneway)
                .for_each(|name, old, new| {
                    output.push((name.clone(), old, new));
                    Ok(())
                })?;

            output.sort_unstable_by_key(|(_, (_, old_size), (_, new_size))| new_size - old_size);

            for (name, (old_addr, old_size), (new_addr, new_size)) in output {
                let diff_size = new_size - old_size;

                change_count += diff_size;

                writeln!(&mut stdout,
                    "{:018p}\t{:018p}\t{}\t{}\t{}\t\t{}",
                    old_addr as *const (),
                    new_addr as *const (),
                    old_size,
                    new_size,
                    diff_size,
                    name.as_bstr()
                )?;
            }
        }

        writeln!(&mut stdout, "total:\t\t\t{}", change_count)?;

        Ok(())
    }
}
