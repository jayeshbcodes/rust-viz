//! `HashMap`: a stack header pointing at a heap hash table, grown exactly
//! like hashbrown (std's implementation) — see `memory::map_capacity`.

use super::*;
use crate::memory::{map_buckets_for, map_capacity, map_order};
use crate::value::{IterNode, MapPart};

impl Interpreter {
    /// A new table sized for `cap` entries (`HashMap::new()` = 0: no heap
    /// allocation until the first insert).
    pub(super) fn map_new(&mut self, cap: usize) -> Value {
        let buckets = map_buckets_for(cap);
        let id = self.heap.alloc(HeapObject::Map { entries: vec![], buckets });
        if buckets > 0 {
            self.pending.push(Event::HeapAlloc { id, what: format!("hash table, {buckets} buckets (room for {} entries)", map_capacity(buckets)) });
        }
        Value::Map(id)
    }

    /// Make room for `additional` more entries — hashbrown's
    /// `reserve_rehash`: a full table is replaced by one with enough
    /// buckets for `max(needed, capacity + 1)`, every entry re-inserted.
    fn map_reserve(&mut self, id: HeapId, additional: usize) -> R<()> {
        let HeapObject::Map { entries, buckets } = self.heap.get_mut(id)? else { return Ok(()) };
        let cap = map_capacity(*buckets);
        if additional <= cap - entries.len() {
            return Ok(());
        }
        let want = (entries.len() + additional).max(cap + 1);
        let new_buckets = map_buckets_for(want);
        let old = *buckets;
        *buckets = new_buckets;
        let new_cap = map_capacity(new_buckets);
        self.pending.push(if old == 0 {
            Event::HeapAlloc { id, what: format!("hash table, {new_buckets} buckets (room for {new_cap} entries)") }
        } else {
            Event::HeapRealloc { id, old_cap: cap, new_cap }
        });
        if old > 0 {
            self.pending.push(Event::Note(format!(
                "the table was full → rehash: a new {new_buckets}-bucket table is allocated, every entry is re-inserted, the old one freed"
            )));
        }
        Ok(())
    }

    /// Index of `key`'s entry. Lookups compare by value, through
    /// references (so `get("Blue")` finds a `String` key `"Blue"`).
    pub(super) fn map_find(&self, id: HeapId, key: &Value) -> R<Option<usize>> {
        let needle = self.deref_val(key.clone())?;
        let HeapObject::Map { entries, .. } = self.heap.get(id)? else { return Ok(None) };
        for (i, (k, _)) in entries.iter().enumerate() {
            let k = self.deref_val(k.clone())?;
            if self.compare(&k, &needle)? == Some(std::cmp::Ordering::Equal) {
                return Ok(Some(i));
            }
        }
        Ok(None)
    }

    /// Inserts, returning the old value if `key` was already present (then
    /// the *new* key is dropped and the original one kept, like std).
    fn map_insert(&mut self, id: HeapId, key: Value, val: Value) -> R<Option<Value>> {
        if let Some(i) = self.map_find(id, &key)? {
            let HeapObject::Map { entries, .. } = self.heap.get_mut(id)? else { unreachable!() };
            let old = std::mem::replace(&mut entries[i].1, val);
            self.drop_value(&key)?;
            return Ok(Some(old));
        }
        self.map_reserve(id, 1)?;
        if let HeapObject::Map { entries, .. } = self.heap.get_mut(id)? {
            entries.push((key, val));
        }
        Ok(None)
    }

    pub(super) fn map_entries(&self, id: HeapId) -> R<Vec<(Value, Value)>> {
        Ok(match self.heap.get(id)? {
            HeapObject::Map { entries, .. } => entries.clone(),
            _ => vec![],
        })
    }

    /// `HashMap::from([(k, v), ...])` / `.collect()` / `.extend()`: reserve
    /// the iterator's lower bound up front, then insert each pair.
    pub(super) fn map_extend(&mut self, id: HeapId, node: &mut IterNode, line: usize) -> R<()> {
        let lower = iters::size_hint(node).0;
        let empty = self.map_entries(id)?.is_empty();
        self.map_reserve(id, if empty { lower } else { lower.div_ceil(2) })?;
        while let Some(item) = self.iter_next(node, line)? {
            let Value::Tuple(mut kv) = item else {
                return compile_err(format!("a `HashMap` is built from `(key, value)` tuples, not `{}` (E0277)", item.type_name()));
            };
            let (Some(v), Some(k)) = (kv.pop(), kv.pop()) else { return compile_err("expected a `(key, value)` tuple (E0308)") };
            if let Some(old) = self.map_insert(id, k, v)? {
                self.drop_value(&old)?;
            }
        }
        Ok(())
    }

    /// Methods on a `HashMap` receiver and on an `Entry`. `None` = not ours.
    pub(super) fn map_method(&mut self, method: &str, recv: &PlaceRef, got: &Got, arg_exprs: &syn::punctuated::Punctuated<Expr, syn::Token![,]>, line: usize) -> R<Option<Value>> {
        if let Got::Val(Value::Struct { ty, .. }) = got
            && ty == "Entry"
        {
            return self.entry_method(method, recv, arg_exprs, line).map(Some);
        }
        let Got::Val(Value::Map(id)) = got else { return Ok(None) };
        let id = *id;
        let mut args = Vec::new();
        for a in arg_exprs {
            args.push(self.eval_arg(a)?);
        }
        let name = recv.place.describe();
        let read = |this: &mut Self| -> R<()> {
            if recv.via.is_none() {
                this.access(recv.place.frame, recv.place.slot, &recv.place.path, Access::Read)?;
            }
            Ok(())
        };
        // Lookup keys are borrowed for the call only.
        let release_args = |this: &mut Self, args: &[Value]| -> R<()> {
            for a in args {
                if matches!(a, Value::Ref(..)) {
                    this.drop_value(a)?;
                }
            }
            Ok(())
        };
        let out = match method {
            "insert" => {
                self.check_write(recv, false)?;
                let mut it = args.into_iter();
                let (k, v) = (it.next().unwrap_or(Value::Unit), it.next().unwrap_or(Value::Unit));
                let old = self.map_insert(id, k, v)?;
                self.pending.push(Event::Mutate { name });
                if old.is_some() {
                    self.pending.push(Event::Note("insert: the key was already there → its value is replaced, and the old value returned".into()));
                }
                old.map_or_else(option_none, option_some)
            }
            "get" | "get_mut" | "contains_key" | "remove" => {
                let key = args.first().cloned().unwrap_or(Value::Unit);
                if matches!(method, "get_mut" | "remove") {
                    self.check_write(recv, false)?;
                } else {
                    read(self)?;
                }
                let found = self.map_find(id, &key)?;
                release_args(self, &args)?;
                match (method, found) {
                    ("contains_key", f) => Value::Bool(f.is_some()),
                    (_, None) => option_none(),
                    ("remove", Some(i)) => {
                        let HeapObject::Map { entries, .. } = self.heap.get_mut(id)? else { unreachable!() };
                        let (k, v) = entries.remove(i);
                        self.drop_value(&k)?;
                        self.pending.push(Event::Mutate { name });
                        option_some(v)
                    }
                    (_, Some(i)) => {
                        let mut p = recv.clone();
                        p.place.path.push(Proj::MapVal(i));
                        option_some(self.borrow_place(p, method == "get_mut")?)
                    }
                }
            }
            "capacity" => {
                read(self)?;
                match self.heap.get(id)? {
                    HeapObject::Map { buckets, .. } => Value::Int(map_capacity(*buckets) as i64),
                    _ => Value::Int(0),
                }
            }
            "clear" => {
                self.check_write(recv, false)?;
                for (k, v) in self.map_entries(id)? {
                    self.drop_value(&k)?;
                    self.drop_value(&v)?;
                }
                if let HeapObject::Map { entries, .. } = self.heap.get_mut(id)? {
                    entries.clear();
                }
                self.pending.push(Event::Mutate { name });
                Value::Unit
            }
            "entry" => {
                // `Entry<'_, K, V>` holds a `&mut` borrow of the map; the
                // `&mut V` that `or_insert` returns carries it on.
                let key = args.into_iter().next().unwrap_or(Value::Unit);
                let r = self.borrow_place(recv.clone(), true)?;
                Value::Struct { ty: "Entry".into(), fields: vec![("map".into(), r), ("key".into(), key)], copy: false }
            }
            "iter" | "iter_mut" | "keys" | "values" | "values_mut" | "into_iter" if method != "into_iter" || recv.via.is_some() => {
                let mutable = matches!(method, "iter_mut" | "values_mut");
                let part = match method {
                    "keys" => MapPart::Keys,
                    "values" | "values_mut" => MapPart::Values,
                    _ => MapPart::Both,
                };
                Value::Iter(Box::new(self.map_iter(recv.clone(), mutable, part)?))
            }
            "into_iter" => {
                let v = self.read_place(recv)?;
                Value::Iter(Box::new(self.node_from(v)?))
            }
            "extend" => {
                self.check_write(recv, false)?;
                let v = args.into_iter().next().unwrap_or(Value::Unit);
                let mut node = self.node_from(v)?;
                self.map_extend(id, &mut node, line)?;
                self.drop_value(&Value::Iter(Box::new(node)))?;
                self.pending.push(Event::Mutate { name });
                Value::Unit
            }
            _ => return Ok(None),
        };
        Ok(Some(out))
    }

    /// An iterator of references into the map's table (borrows it).
    pub(super) fn map_iter(&mut self, recv: PlaceRef, mutable: bool, part: MapPart) -> R<IterNode> {
        let r = self.borrow_place(recv, mutable)?;
        let Value::Ref(base, kind) = r.clone() else { unreachable!() };
        let Got::Val(Value::Map(id)) = self.got(&base)? else { unreachable!() };
        let entries = self.map_entries(id)?;
        let order = map_order(&self.stack, &self.heap, &entries);
        Ok(IterNode::MapIter { base: Place { reborrow: true, ..base }, borrow: Box::new(r), order, front: 0, kind, part })
    }

    /// `map.entry(k).or_insert(v)` and friends.
    fn entry_method(&mut self, method: &str, recv: &PlaceRef, arg_exprs: &syn::punctuated::Punctuated<Expr, syn::Token![,]>, line: usize) -> R<Value> {
        let mut args = Vec::new();
        for a in arg_exprs {
            args.push(self.eval_arg(a)?);
        }
        let Value::Struct { fields, .. } = self.read_place(recv)? else { unreachable!() };
        let mut fields = fields.into_iter();
        let (Some((_, map_ref)), Some((_, key))) = (fields.next(), fields.next()) else { unreachable!() };
        let Value::Ref(map_place, _) = &map_ref else { unreachable!() };
        let map_place = map_place.clone();
        let Got::Val(Value::Map(id)) = self.got(&map_place)? else { unreachable!() };
        let found = self.map_find(id, &key)?;
        let val_ref = |i: usize, reborrow: bool| {
            let mut p = map_place.clone();
            p.path.push(Proj::MapVal(i));
            p.reborrow = reborrow;
            Value::Ref(p, RefKind::Mutable)
        };
        match method {
            "and_modify" => {
                let mut f = args.into_iter().next().unwrap_or(Value::Unit);
                if let Some(i) = found {
                    self.pending.push(Event::Note("entry: key present → and_modify runs on its value".into()));
                    let r = self.call_value(&mut f, vec![val_ref(i, true)], line, crate::value::ClosureKind::FnOnce)?;
                    self.drop_value(&r)?;
                }
                self.drop_value(&f)?;
                Ok(Value::Struct { ty: "Entry".into(), fields: vec![("map".into(), map_ref), ("key".into(), key)], copy: false })
            }
            "or_insert" | "or_insert_with" | "or_default" => {
                let i = match found {
                    Some(i) => {
                        self.pending.push(Event::Note("entry: key already present → nothing inserted; returns `&mut` to the existing value".into()));
                        self.drop_value(&key)?;
                        for a in &args {
                            self.drop_value(a)?;
                        }
                        i
                    }
                    None => {
                        let v = match method {
                            "or_insert" => args.into_iter().next().unwrap_or(Value::Unit),
                            "or_insert_with" => {
                                let mut f = args.into_iter().next().unwrap_or(Value::Unit);
                                self.call_value(&mut f, vec![], line, crate::value::ClosureKind::FnOnce)?
                            }
                            _ => self.default_like(id)?,
                        };
                        self.pending.push(Event::Note("entry: key absent → inserted; returns `&mut` to the new value".into()));
                        self.map_insert(id, key, v)?;
                        self.pending.push(Event::Mutate { name: map_place.describe() });
                        self.map_entries(id)?.len() - 1
                    }
                };
                // The entry's `&mut` borrow of the map passes to this `&mut V`.
                Ok(val_ref(i, map_place.reborrow))
            }
            "key" => {
                self.drop_value(&map_ref)?;
                Ok(key)
            }
            other => Err(self.unsupported(format!("`Entry::{other}`"))),
        }
    }

    /// `or_default()`: the value type's default, guessed from the map's
    /// existing values (we don't track static types); `0` if it's empty.
    fn default_like(&mut self, id: HeapId) -> R<Value> {
        let sample = self.map_entries(id)?.into_iter().next().map(|(_, v)| v);
        Ok(match sample {
            Some(Value::Float(_)) => Value::Float(0.0),
            Some(Value::Bool(_)) => Value::Bool(false),
            Some(Value::Str(_)) => self.alloc_str(String::new()),
            Some(Value::Vec(_)) => self.alloc_vec(vec![], 0),
            _ => Value::Int(0),
        })
    }
}
