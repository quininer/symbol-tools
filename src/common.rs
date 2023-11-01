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
