use std::rc::Rc;
use std::collections::HashMap;
use object::{ Symbol, SymbolKind };
use rustc_demangle::demangle;


pub fn collect_map(symbols: &[Symbol<'_>]) -> HashMap<Rc<[u8]>, (u64, u64)> {
    let mut map: HashMap<Rc<[u8]>, (u64, u64)> = HashMap::new();

    for symbol in symbols
            .iter()
            .filter(|symbol| symbol.kind() == SymbolKind::Text)
    {
        if let Some(name) = symbol.name()
            .filter(|name| !name.is_empty())
            .map(|name| format!("{:#}", demangle(name))) // TODO strip tail
            .map(|name| Rc::from(name.into_bytes().into_boxed_slice()))
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
