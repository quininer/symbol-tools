use std::rc::Rc;
use std::collections::HashMap;
use bstr::ByteSlice;
use object::{ Symbol, SymbolKind, ObjectSymbol };
use rustc_demangle::demangle;


pub fn collect_map<'data, T: 'data>(symbols: T, filter_outlined: bool)
    -> HashMap<Rc<[u8]>, (u64, u64)>
where
    T: Iterator<Item = Symbol<'data, 'data>>
{
    let mut map: HashMap<Rc<[u8]>, (u64, u64)> = HashMap::new();
    let outlined_name = Rc::from("OUTLINED_FUNCTION_".as_bytes());

    for symbol in symbols
            .filter(|symbol| symbol.kind() == SymbolKind::Text)
    {
        if let Some(name) = symbol.name()
            .ok()
            .filter(|name| !name.is_empty())
            .map(|name| format!("{:#}", demangle(name)))
            .map(|name| if filter_outlined && name.as_bytes().starts_with_str(&outlined_name) {
                Rc::clone(&outlined_name)
            } else {
                Rc::from(name.into_bytes().into_boxed_slice())
            })
        {
            let addr = symbol.address();
            let size = symbol.size();

            map.entry(name)
                .and_modify(|entry| entry.1 += size)
                .or_insert_with(|| (addr, size));
        }
    }

    map.shrink_to_fit();
    map
}

pub trait IteratorExt: Iterator {
    fn flat_result<U, T, E>(self) -> FlatResultIter<Self, U, T, E>
    where
        Self: Iterator<Item = Result<U, E>> + Sized,
        U: IntoIterator<Item = Result<T, E>>
    {
        FlatResultIter {
            iter: self,
            subiter: None,
            _phantom: Default::default()
        }
    }

    fn fast_for_each<F, E>(mut self, f: F)
        -> Result<(), E>
    where
        Self: Sized + Send,
        Self::Item: Send,
        F: Fn(Self::Item) -> Result<(), E> + Send + Sync,
        E: Send
    {
        use rayon::prelude::*;

        let (lower_bound, _) = self.size_hint();

        if lower_bound > 1024 * 1024 {
            self.par_bridge().try_for_each(f)
        } else {
            self.try_for_each(f)
        }
    }
}

impl<I: Iterator> IteratorExt for I {}

pub struct FlatResultIter<I, U, T, E>
where
    U: IntoIterator<Item = Result<T, E>>
{
    iter: I,
    subiter: Option<U::IntoIter>,
    _phantom: std::marker::PhantomData<(T, E)>
}

impl<I, U, T, E> Iterator for FlatResultIter<I, U, T, E>
where
    I: Iterator<Item = Result<U, E>>,
    U: IntoIterator<Item = Result<T, E>>
{
    type Item = Result<T, E>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.subiter.is_none() {
                self.subiter = match self.iter.next() {
                    Some(Ok(iter)) => Some(iter.into_iter()),
                    Some(Err(err)) => return Some(Err(err)),
                    None => None
                };
            }

            match self.subiter.as_mut()?.next() {
                Some(output) => return Some(output),
                None => self.subiter = None
            }
        }
    }
}

pub enum DoubleLife<'a, 'b, T: ?Sized> {
    Left(&'a T),
    Right(&'b T)
}

impl<'a, 'b, T: ?Sized> AsRef<T> for DoubleLife<'a, 'b, T> {
    fn as_ref(&self) -> &T {
        match self {
            DoubleLife::Left(t) => t,
            DoubleLife::Right(t) => t
        }
    }
}

pub fn data_range<'data>(
    data: &'data [u8],
    data_address: u64,
    range_address: u64,
    size: u64
)
    -> anyhow::Result<&'data [u8]>
{
    use std::convert::TryInto;
    use anyhow::Context;

    let address = range_address.checked_sub(data_address).context("bad address")?;
    let offset: usize = address.try_into().context("symbol address cast fail")?;
    let size: usize = size.try_into().context("symbol size cast fail")?;

    data.get(offset..)
        .and_then(|data| data.get(..size))
        .context("section range overflow")
}

pub fn print_pretty_bytes(
    stdout: &mut dyn std::io::Write,
    base: u64,
    bytes: &[u8],
) -> anyhow::Result<()> {
    use std::fmt;

    struct HexPrinter<'a>(&'a [u8]);
    struct AsciiPrinter<'a>(&'a [u8]);

    impl fmt::Display for HexPrinter<'_> {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            for &b in self.0.iter() {
                write!(f, "{:02x} ", b)?;
            }

            for _ in self.0.len()..16 {
                write!(f, "   ")?;
            }

            Ok(())
        }
    }

    impl fmt::Display for AsciiPrinter<'_> {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            use std::fmt::Write;

            for &b in self.0.iter() {
                let c = b as char;
                let c = if c.is_ascii_graphic() {
                    c
                } else {
                    '.'
                };
                f.write_char(c)?;
            }

            Ok(())
        }
    }

    let addr = base as *const u8;

    for (offset, chunk) in bytes.chunks(16).enumerate() {
        let addr = addr.wrapping_add(offset * 16);

        writeln!(
            stdout,
            "{:018p}: {} {}",
            addr,
            HexPrinter(chunk),
            AsciiPrinter(chunk)
        )?;
    }

    Ok(())
}
