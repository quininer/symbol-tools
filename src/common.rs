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
