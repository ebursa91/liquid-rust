//! Type representing a Liquid object, payload of the `Value::Object` variant

use std::borrow::Borrow;
use std::collections::hash_map;
use std::fmt::{self, Debug};
use std::hash::Hash;
use std::iter::FromIterator;
use std::ops;
use std::sync::{Arc, LazyLock};

use serde::{de, ser};

use super::Value;

/// Type representing a Liquid object, payload of the `Value::Object` variant
///
/// Cloning shares immutable storage. Mutating either object keeps the other
/// object's values unchanged, including nested objects. New empty objects defer
/// storage allocation until it is needed for mutation.
#[derive(Default, Clone, Eq)]
pub struct Object {
    map: Option<Arc<MapImpl<Key, Value>>>,
}

type Key = crate::model::KString;

type MapImpl<K, V> = hash_map::HashMap<K, V>;
// Borrowed empty iterators keep their existing concrete HashMap-backed types.
// Each first mutation creates a fresh map instead of cloning this shared reader.
static EMPTY_MAP: LazyLock<MapImpl<Key, Value>> = LazyLock::new(MapImpl::new);

type VacantEntryImpl<'a> = hash_map::VacantEntry<'a, Key, Value>;
type OccupiedEntryImpl<'a> = hash_map::OccupiedEntry<'a, Key, Value>;
type IterImpl<'a> = hash_map::Iter<'a, Key, Value>;
type IterMutImpl<'a> = hash_map::IterMut<'a, Key, Value>;
type IntoIterImpl = hash_map::IntoIter<Key, Value>;
type KeysImpl<'a> = hash_map::Keys<'a, Key, Value>;
type ValuesImpl<'a> = hash_map::Values<'a, Key, Value>;
type ValuesMutImpl<'a> = hash_map::ValuesMut<'a, Key, Value>;

impl PartialEq for Object {
    fn eq(&self, other: &Self) -> bool {
        // Compare values even when storage is shared: Liquid scalars can contain
        // NaN, whose existing equality must not become true via Arc identity.
        self.as_map() == other.as_map()
    }
}

impl Object {
    /// Makes a new empty Object.
    #[inline]
    pub fn new() -> Self {
        Object { map: None }
    }

    #[inline]
    fn as_map(&self) -> &MapImpl<Key, Value> {
        match self.map.as_deref() {
            Some(map) => map,
            None => &EMPTY_MAP,
        }
    }

    #[inline]
    fn as_map_mut(&mut self) -> &mut MapImpl<Key, Value> {
        Arc::make_mut(self.map.get_or_insert_with(|| Arc::new(MapImpl::new())))
    }

    /// Clears the map, removing all values.
    #[inline]
    pub fn clear(&mut self) {
        if let Some(map) = &mut self.map {
            Arc::make_mut(map).clear();
        }
    }

    /// Returns a reference to the value corresponding to the key.
    ///
    /// The key may be any borrowed form of the map's key type, but the ordering
    /// on the borrowed form *must* match the ordering on the key type.
    #[inline]
    pub fn get<Q>(&self, key: &Q) -> Option<&Value>
    where
        Key: Borrow<Q>,
        Q: Ord + Eq + Hash + ?Sized,
    {
        self.map.as_ref()?.get(key)
    }

    /// Returns true if the map contains a value for the specified key.
    ///
    /// The key may be any borrowed form of the map's key type, but the ordering
    /// on the borrowed form *must* match the ordering on the key type.
    #[inline]
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        Key: Borrow<Q>,
        Q: Ord + Eq + Hash + ?Sized,
    {
        self.map.as_ref().is_some_and(|map| map.contains_key(key))
    }

    /// Returns a mutable reference to the value corresponding to the key.
    ///
    /// The key may be any borrowed form of the map's key type, but the ordering
    /// on the borrowed form *must* match the ordering on the key type.
    #[inline]
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut Value>
    where
        Key: Borrow<Q>,
        Q: Ord + Eq + Hash + ?Sized,
    {
        Arc::make_mut(self.map.as_mut()?).get_mut(key)
    }

    /// Inserts a key-value pair into the map.
    ///
    /// If the map did not have this key present, `None` is returned.
    ///
    /// If the map did have this key present, the value is updated, and the old
    /// value is returned.
    #[inline]
    pub fn insert(&mut self, k: Key, v: Value) -> Option<Value> {
        self.as_map_mut().insert(k, v)
    }

    /// Removes a key from the map, returning the value at the key if the key
    /// was previously in the map.
    ///
    /// The key may be any borrowed form of the map's key type, but the ordering
    /// on the borrowed form *must* match the ordering on the key type.
    #[inline]
    pub fn remove<Q>(&mut self, key: &Q) -> Option<Value>
    where
        Key: Borrow<Q>,
        Q: Ord + Eq + Hash + ?Sized,
    {
        Arc::make_mut(self.map.as_mut()?).remove(key)
    }

    /// Gets the given key's corresponding entry in the map for in-place
    /// manipulation.
    pub fn entry<S>(&mut self, key: S) -> Entry<'_>
    where
        S: Into<Key>,
    {
        use std::collections::hash_map::Entry as EntryImpl;
        match self.as_map_mut().entry(key.into()) {
            EntryImpl::Vacant(vacant) => Entry::Vacant(VacantEntry { vacant }),
            EntryImpl::Occupied(occupied) => Entry::Occupied(OccupiedEntry { occupied }),
        }
    }

    /// Returns the number of elements in the map.
    #[inline]
    pub fn len(&self) -> usize {
        self.map.as_ref().map(|map| map.len()).unwrap_or(0)
    }

    /// Returns true if the map contains no elements.
    #[inline]
    pub fn is_empty(&self) -> bool {
        match &self.map {
            Some(map) => map.is_empty(),
            None => true,
        }
    }

    /// Gets an iterator over the entries of the map.
    #[inline]
    pub fn iter(&self) -> Iter<'_> {
        Iter {
            iter: self.as_map().iter(),
        }
    }

    /// Gets a mutable iterator over the entries of the map.
    #[inline]
    pub fn iter_mut(&mut self) -> IterMut<'_> {
        IterMut {
            iter: self.as_map_mut().iter_mut(),
        }
    }

    /// Gets an iterator over the keys of the map.
    #[inline]
    pub fn keys(&self) -> Keys<'_> {
        Keys {
            iter: self.as_map().keys(),
        }
    }

    /// Gets an iterator over the values of the map.
    #[inline]
    pub fn values(&self) -> Values<'_> {
        Values {
            iter: self.as_map().values(),
        }
    }

    /// Gets an iterator over mutable values of the map.
    #[inline]
    pub fn values_mut(&mut self) -> ValuesMut<'_> {
        ValuesMut {
            iter: self.as_map_mut().values_mut(),
        }
    }
}

/// Access an element of this map. Panics if the given key is not present in the
/// map.
///
/// ```rust
/// # use liquid_core::model::Value;
/// # use liquid_core::model::ValueView;
/// #
/// # let val = &Value::scalar("");
/// # let _ =
/// match *val {
///     Value::Scalar(ref s) => Some(s.to_kstr()),
///     Value::Array(ref arr) => Some(arr[0].to_kstr()),
///     Value::Object(ref map) => Some(map["type"].to_kstr()),
///     _ => None,
/// }
/// # ;
/// ```
impl<Q: ?Sized> ops::Index<&Q> for Object
where
    Key: Borrow<Q>,
    Q: Ord + Eq + Hash,
{
    type Output = Value;

    fn index(&self, index: &Q) -> &Value {
        self.as_map().index(index)
    }
}

/// Mutably access an element of this map. Panics if the given key is not
/// present in the map.
///
/// ```rust
/// #     let mut map = liquid_core::model::Object::new();
/// #     map.insert("key".into(), liquid_core::model::Value::Nil);
/// #
/// map["key"] = liquid_core::value!("value");
/// ```
impl<Q: ?Sized> ops::IndexMut<&Q> for Object
where
    Key: Borrow<Q>,
    Q: Ord + Eq + Hash,
{
    fn index_mut(&mut self, index: &Q) -> &mut Value {
        self.as_map_mut()
            .get_mut(index)
            .expect("no entry found for key")
    }
}

impl Debug for Object {
    #[inline]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        self.as_map().fmt(formatter)
    }
}

impl ser::Serialize for Object {
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: ser::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (k, v) in self {
            map.serialize_key(k)?;
            map.serialize_value(v)?;
        }
        map.end()
    }
}

impl<'de> de::Deserialize<'de> for Object {
    #[inline]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Object;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a map")
            }

            #[inline]
            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(Object::new())
            }

            #[inline]
            fn visit_map<V>(self, mut visitor: V) -> Result<Self::Value, V::Error>
            where
                V: de::MapAccess<'de>,
            {
                let mut values = Object::new();

                while let Some((key, value)) = visitor.next_entry()? {
                    values.insert(key, value);
                }

                Ok(values)
            }
        }

        deserializer.deserialize_map(Visitor)
    }
}

impl FromIterator<(Key, Value)> for Object {
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = (Key, Value)>,
    {
        let map: MapImpl<Key, Value> = FromIterator::from_iter(iter);
        Self {
            map: if map.is_empty() {
                None
            } else {
                Some(Arc::new(map))
            },
        }
    }
}

impl Extend<(Key, Value)> for Object {
    fn extend<T>(&mut self, iter: T)
    where
        T: IntoIterator<Item = (Key, Value)>,
    {
        self.as_map_mut().extend(iter);
    }
}

macro_rules! delegate_iterator {
    (($name:ident $($generics:tt)*) => $item:ty) => {
        impl $($generics)* Iterator for $name $($generics)* {
            type Item = $item;
            #[inline]
            fn next(&mut self) -> Option<Self::Item> {
                self.iter.next()
            }
            #[inline]
            fn size_hint(&self) -> (usize, Option<usize>) {
                self.iter.size_hint()
            }
        }

        impl $($generics)* ExactSizeIterator for $name $($generics)* {
            #[inline]
            fn len(&self) -> usize {
                self.iter.len()
            }
        }
    }
}

//////////////////////////////////////////////////////////////////////////////

/// A view into a single entry in a map, which may either be vacant or occupied.
/// This enum is constructed from the [`entry`] method on [`Object`].
///
/// [`entry`]: Object::entry()
#[derive(Debug)]
pub enum Entry<'a> {
    /// A vacant Entry.
    Vacant(VacantEntry<'a>),
    /// An occupied Entry.
    Occupied(OccupiedEntry<'a>),
}

/// A vacant Entry. It is part of the [`Entry`] enum.
///
#[derive(Debug)]
pub struct VacantEntry<'a> {
    vacant: VacantEntryImpl<'a>,
}

/// An occupied Entry. It is part of the [`Entry`] enum.
///
#[derive(Debug)]
pub struct OccupiedEntry<'a> {
    occupied: OccupiedEntryImpl<'a>,
}

impl<'a> Entry<'a> {
    /// Returns a reference to this entry's key.
    ///
    /// # Examples
    ///
    /// ```rust
    /// let mut map = liquid_core::model::Object::new();
    /// assert_eq!(map.entry("liquid").key(), &"liquid");
    /// ```
    pub fn key(&self) -> &Key {
        match *self {
            Entry::Vacant(ref e) => e.key(),
            Entry::Occupied(ref e) => e.key(),
        }
    }

    /// Ensures a value is in the entry by inserting the default if empty, and
    /// returns a mutable reference to the value in the entry.
    ///
    /// # Examples
    ///
    /// ```rust
    /// let mut map = liquid_core::model::Object::new();
    /// map.entry("liquid").or_insert(liquid_core::value!(12));
    ///
    /// assert_eq!(map["liquid"], liquid_core::value!(12));
    /// ```
    pub fn or_insert(self, default: Value) -> &'a mut Value {
        match self {
            Entry::Vacant(entry) => entry.insert(default),
            Entry::Occupied(entry) => entry.into_mut(),
        }
    }

    /// Ensures a value is in the entry by inserting the result of the default
    /// function if empty, and returns a mutable reference to the value in the
    /// entry.
    ///
    /// # Examples
    ///
    /// ```rust
    /// let mut map = liquid_core::model::Object::new();
    /// map.entry("liquid").or_insert_with(|| liquid_core::value!("hoho"));
    ///
    /// assert_eq!(map["liquid"], liquid_core::value!("hoho"));
    /// ```
    pub fn or_insert_with<F>(self, default: F) -> &'a mut Value
    where
        F: FnOnce() -> Value,
    {
        match self {
            Entry::Vacant(entry) => entry.insert(default()),
            Entry::Occupied(entry) => entry.into_mut(),
        }
    }
}

impl<'a> VacantEntry<'a> {
    /// Gets a reference to the key that would be used when inserting a value
    /// through the VacantEntry.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use liquid_core::model::map::Entry;
    ///
    /// let mut map = liquid_core::model::Object::new();
    ///
    /// match map.entry("liquid") {
    ///     Entry::Vacant(vacant) => {
    ///         assert_eq!(vacant.key(), &"liquid");
    ///     }
    ///     Entry::Occupied(_) => unimplemented!(),
    /// }
    /// ```
    #[inline]
    pub fn key(&self) -> &Key {
        self.vacant.key()
    }

    /// Sets the value of the entry with the VacantEntry's key, and returns a
    /// mutable reference to it.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use liquid_core::model::map::Entry;
    ///
    /// let mut map = liquid_core::model::Object::new();
    ///
    /// match map.entry("liquid") {
    ///     Entry::Vacant(vacant) => {
    ///         vacant.insert(liquid_core::value!("hoho"));
    ///     }
    ///     Entry::Occupied(_) => unimplemented!(),
    /// }
    /// ```
    #[inline]
    pub fn insert(self, value: Value) -> &'a mut Value {
        self.vacant.insert(value)
    }
}

impl<'a> OccupiedEntry<'a> {
    /// Gets a reference to the key in the entry.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use liquid_core::model::map::Entry;
    ///
    /// let mut map = liquid_core::model::Object::new();
    /// map.insert("liquid".into(), liquid_core::value!(12));
    ///
    /// match map.entry("liquid") {
    ///     Entry::Occupied(occupied) => {
    ///         assert_eq!(occupied.key(), &"liquid");
    ///     }
    ///     Entry::Vacant(_) => unimplemented!(),
    /// }
    /// ```
    #[inline]
    pub fn key(&self) -> &Key {
        self.occupied.key()
    }

    /// Gets a reference to the value in the entry.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use liquid_core::model::map::Entry;
    ///
    /// let mut map = liquid_core::model::Object::new();
    /// map.insert("liquid".into(), liquid_core::value!(12));
    ///
    /// match map.entry("liquid") {
    ///     Entry::Occupied(occupied) => {
    ///         assert_eq!(occupied.get(), &liquid_core::value!(12));
    ///     }
    ///     Entry::Vacant(_) => unimplemented!(),
    /// }
    /// ```
    #[inline]
    pub fn get(&self) -> &Value {
        self.occupied.get()
    }

    /// Gets a mutable reference to the value in the entry.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use liquid_core::model::ValueView;
    /// #
    /// use liquid_core::model::map::Entry;
    ///
    /// let mut map = liquid_core::model::Object::new();
    /// map.insert("liquid".into(), liquid_core::value!([1, 2, 3]));
    ///
    /// match map.entry("liquid") {
    ///     Entry::Occupied(mut occupied) => {
    ///         occupied.get_mut().as_array().unwrap();
    ///     }
    ///     Entry::Vacant(_) => unimplemented!(),
    /// }
    /// ```
    #[inline]
    pub fn get_mut(&mut self) -> &mut Value {
        self.occupied.get_mut()
    }

    /// Converts the entry into a mutable reference to its value.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use liquid_core::model::ValueView;
    /// #
    /// use liquid_core::model::map::Entry;
    ///
    /// let mut map = liquid_core::model::Object::new();
    /// map.insert("liquid".into(), liquid_core::value!([1, 2, 3]));
    ///
    /// match map.entry("liquid") {
    ///     Entry::Occupied(mut occupied) => {
    ///         occupied.into_mut().as_array().unwrap();
    ///     }
    ///     Entry::Vacant(_) => unimplemented!(),
    /// }
    /// ```
    #[inline]
    pub fn into_mut(self) -> &'a mut Value {
        self.occupied.into_mut()
    }

    /// Sets the value of the entry with the `OccupiedEntry`'s key, and returns
    /// the entry's old value.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use liquid_core::model::map::Entry;
    ///
    /// let mut map = liquid_core::model::Object::new();
    /// map.insert("liquid".into(), liquid_core::value!(12));
    ///
    /// match map.entry("liquid") {
    ///     Entry::Occupied(mut occupied) => {
    ///         assert_eq!(occupied.insert(liquid_core::value!(13)), liquid_core::value!(12));
    ///         assert_eq!(occupied.get(), &liquid_core::value!(13));
    ///     }
    ///     Entry::Vacant(_) => unimplemented!(),
    /// }
    /// ```
    #[inline]
    pub fn insert(&mut self, value: Value) -> Value {
        self.occupied.insert(value)
    }

    /// Takes the value of the entry out of the map, and returns it.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use liquid_core::model::map::Entry;
    ///
    /// let mut map = liquid_core::model::Object::new();
    /// map.insert("liquid".into(), liquid_core::value!(12));
    ///
    /// match map.entry("liquid") {
    ///     Entry::Occupied(occupied) => {
    ///         assert_eq!(occupied.remove(), liquid_core::value!(12));
    ///     }
    ///     Entry::Vacant(_) => unimplemented!(),
    /// }
    /// ```
    #[inline]
    pub fn remove(self) -> Value {
        self.occupied.remove()
    }
}

//////////////////////////////////////////////////////////////////////////////

impl<'a> IntoIterator for &'a Object {
    type Item = (&'a Key, &'a Value);
    type IntoIter = Iter<'a>;
    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        Iter {
            iter: self.as_map().iter(),
        }
    }
}

/// An iterator over a liquid_core::model::Object's entries.
#[derive(Debug)]
pub struct Iter<'a> {
    iter: IterImpl<'a>,
}

delegate_iterator!((Iter<'a>) => (&'a Key, &'a Value));

//////////////////////////////////////////////////////////////////////////////

impl<'a> IntoIterator for &'a mut Object {
    type Item = (&'a Key, &'a mut Value);
    type IntoIter = IterMut<'a>;
    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        IterMut {
            iter: self.as_map_mut().iter_mut(),
        }
    }
}

/// A mutable iterator over a liquid_core::model::Object's entries.
#[derive(Debug)]
pub struct IterMut<'a> {
    iter: IterMutImpl<'a>,
}

delegate_iterator!((IterMut<'a>) => (&'a Key, &'a mut Value));

//////////////////////////////////////////////////////////////////////////////

impl IntoIterator for Object {
    type Item = (Key, Value);
    type IntoIter = IntoIter;
    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        IntoIter {
            iter: self
                .map
                .map(Arc::unwrap_or_clone)
                .unwrap_or_default()
                .into_iter(),
        }
    }
}

/// An owning iterator over a liquid_core::model::Object's entries.
#[derive(Debug)]
pub struct IntoIter {
    iter: IntoIterImpl,
}

delegate_iterator!((IntoIter) => (Key, Value));

//////////////////////////////////////////////////////////////////////////////

/// An iterator over a liquid_core::model::Object's keys.
#[derive(Debug)]
pub struct Keys<'a> {
    iter: KeysImpl<'a>,
}

delegate_iterator!((Keys<'a>) => &'a Key);

//////////////////////////////////////////////////////////////////////////////

/// An iterator over a liquid_core::model::Object's values.
#[derive(Debug)]
pub struct Values<'a> {
    iter: ValuesImpl<'a>,
}

delegate_iterator!((Values<'a>) => &'a Value);

//////////////////////////////////////////////////////////////////////////////

/// A mutable iterator over a liquid_core::model::Object's values.
#[derive(Debug)]
pub struct ValuesMut<'a> {
    iter: ValuesMutImpl<'a>,
}

delegate_iterator!((ValuesMut<'a>) => &'a mut Value);

#[cfg(test)]
mod tests {
    use super::{Entry, Object};
    use crate::model::{State, Value, ValueCow, ValueView};
    use crate::runtime::{Runtime, RuntimeBuilder};

    fn shares_allocated_storage(left: &Object, right: &Object) -> bool {
        match (&left.map, &right.map) {
            (Some(left), Some(right)) => std::sync::Arc::ptr_eq(left, right),
            _ => false,
        }
    }

    fn sample() -> Object {
        crate::object!({"first": 1, "second": 2})
    }

    fn compound() -> Object {
        crate::object!({
            "items": [{"title": "original", "tags": ["one", "two"]}],
            "nested": {"count": 3},
            "nil": nil,
        })
    }

    fn change_item(value: &mut Value) {
        let Value::Array(items) = value else {
            panic!("Expected items array");
        };
        let Value::Object(item) = &mut items[0] else {
            panic!("Expected item object");
        };
        item.insert("title".into(), Value::scalar("changed"));
        let Value::Array(tags) = item.get_mut("tags").unwrap() else {
            panic!("Expected tags array");
        };
        tags[0] = Value::scalar("changed tag");
    }

    #[test]
    fn clones_share_reads_and_preserve_independent_values() {
        let original = compound();
        let mut copy = original.clone();
        assert!(shares_allocated_storage(&original, &copy));
        assert_eq!(copy, original);
        assert_eq!(copy.render().to_string(), original.render().to_string());
        assert_eq!(copy.source().to_string(), original.source().to_string());
        assert_eq!(format!("{copy:?}"), format!("{original:?}"));

        copy.insert("new".into(), Value::scalar(4));
        assert!(!original.contains_key("new"));
        assert_eq!(copy.get("new"), Some(&Value::scalar(4)));
    }

    #[test]
    fn shared_nan_objects_compare_values_instead_of_allocation_identity() {
        let original = crate::object!({"number": f64::NAN});
        let copy = original.clone();
        assert!(shares_allocated_storage(&original, &copy));
        assert_ne!(original, copy);
        let same_object = &original;
        assert!(!original.eq(same_object));

        let raw: super::MapImpl<super::Key, Value> =
            [("number".into(), Value::scalar(f64::NAN))].into();
        let same_map = &raw;
        assert!(!raw.eq(same_map));
        assert_ne!(raw, raw.clone());
    }

    #[test]
    fn independent_nan_objects_keep_existing_float_equality() {
        let left = crate::object!({"number": f64::NAN});
        let right = crate::object!({"number": f64::NAN});
        assert!(!shares_allocated_storage(&left, &right));
        assert_ne!(left, right);
        assert_eq!(
            crate::object!({"number": f64::INFINITY}),
            crate::object!({"number": f64::INFINITY})
        );
        assert_eq!(
            crate::object!({"number": 0.0}),
            crate::object!({"number": -0.0})
        );
    }

    #[test]
    fn nested_nan_objects_and_arrays_remain_unequal_after_cloning() {
        let nan = crate::object!({"number": f64::NAN});
        for original in [
            crate::object!({"nested": nan.clone()}),
            crate::object!({"items": [nan.clone()]}),
            crate::object!({"items": [f64::NAN]}),
        ] {
            let mut copy = original.clone();
            assert_ne!(original, copy);
            assert_ne!(Value::Object(original.clone()), Value::Object(copy.clone()));
            // Detaching the outer map must not change equality of its payload.
            copy.insert("temporary".into(), Value::Nil);
            copy.remove("temporary");
            assert!(!shares_allocated_storage(&original, &copy));
            assert_ne!(original, copy);
        }
    }

    #[test]
    fn empty_construction_and_collection_keep_clone_independence() {
        for original in [
            Object::new(),
            Object::default(),
            std::iter::empty().collect(),
        ] {
            let mut copy = original.clone();
            assert!(copy.is_empty());
            assert_eq!(copy.len(), 0);
            copy.insert("first".into(), Value::scalar(1));
            assert!(original.is_empty());
            assert_eq!(copy.len(), 1);
        }
        let object = [
            ("first".into(), Value::scalar(1)),
            ("second".into(), Value::scalar(2)),
        ]
        .into_iter()
        .collect::<Object>();
        assert_eq!(object, sample());
    }

    #[test]
    fn lazy_empty_reads_and_clones_do_not_create_object_storage() {
        for object in [
            Object::new(),
            Object::default(),
            std::iter::empty().collect(),
            serde_yaml::from_str::<Object>("{}").unwrap(),
        ] {
            assert!(object.map.is_none());
            assert_eq!(object.len(), 0);
            assert!(object.is_empty());
            assert!(!object.contains_key("missing"));
            assert!(object.get("missing").is_none());
            assert_eq!(object.iter().count(), 0);
            assert_eq!((&object).into_iter().count(), 0);
            assert_eq!(object.keys().count(), 0);
            assert_eq!(object.values().count(), 0);
            assert_eq!(format!("{object:?}"), "{}");
            assert_eq!(serde_yaml::to_string(&object).unwrap().trim(), "---\n{}");
            assert_eq!(object, Object::new());
            assert!(object.query_state(State::Truthy));
            assert!(object.query_state(State::Empty));
            assert!(object.query_state(State::Blank));
            assert!(object.query_state(State::DefaultValue));
            let copy = object.clone();
            assert!(copy.map.is_none());
            assert!(object.map.is_none());
        }
    }

    #[test]
    fn lazy_empty_clear_and_missing_mutations_do_not_create_storage() {
        let mut object = Object::new();
        let snapshot = object.clone();
        object.clear();
        assert!(object.get_mut("missing").is_none());
        assert!(object.remove("missing").is_none());
        assert!(object.map.is_none());
        assert!(snapshot.map.is_none());
        assert_eq!(object, snapshot);
    }

    #[test]
    fn first_writes_after_empty_cloning_create_independent_maps() {
        let original = Object::new();
        let mut inserted = original.clone();
        let mut entered = original.clone();
        inserted.insert("first".into(), Value::scalar(1));
        let Entry::Vacant(entry) = entered.entry("first") else {
            panic!("Expected vacant entry");
        };
        *entry.insert(Value::scalar(1)) = Value::scalar(2);
        assert!(original.map.is_none());
        assert_eq!(inserted, crate::object!({"first": 1}));
        assert_eq!(entered, crate::object!({"first": 2}));
        assert!(!shares_allocated_storage(&inserted, &entered));

        let snapshot = entered.clone();
        assert!(shares_allocated_storage(&entered, &snapshot));
        entered["first"] = Value::scalar(3);
        assert_eq!(snapshot["first"], Value::scalar(2));
        assert!(!shares_allocated_storage(&entered, &snapshot));
    }

    #[test]
    fn lazy_and_allocated_empty_objects_compare_by_values() {
        let lazy = Object::new();
        let mut cleared = sample();
        cleared.clear();
        let mut mutable_empty = Object::new();
        assert_eq!(mutable_empty.iter_mut().count(), 0);
        assert!(lazy.map.is_none());
        assert!(cleared.map.is_some());
        assert!(mutable_empty.map.is_some());
        for allocated in [cleared, mutable_empty] {
            assert_eq!(lazy, allocated);
            assert_eq!(allocated, lazy);
            assert_eq!(
                Value::Object(lazy.clone()),
                Value::Object(allocated.clone())
            );
            assert_eq!(
                crate::object!({"nested": lazy.clone()}),
                crate::object!({"nested": allocated})
            );
        }
    }

    #[test]
    fn empty_iterators_preserve_public_types_and_exact_lengths() {
        let object = Object::new();
        let mut iter: super::Iter<'_> = (&object).into_iter();
        assert_eq!(iter.len(), 0);
        assert_eq!(iter.size_hint(), (0, Some(0)));
        assert!(iter.next().is_none());
        let mut keys: super::Keys<'_> = object.keys();
        assert_eq!(keys.len(), 0);
        assert_eq!(keys.size_hint(), (0, Some(0)));
        assert!(keys.next().is_none());
        let mut values: super::Values<'_> = object.values();
        assert_eq!(values.len(), 0);
        assert_eq!(values.size_hint(), (0, Some(0)));
        assert!(values.next().is_none());
        assert!(object.map.is_none());

        let mut mutable = object.clone();
        let mut iter: super::IterMut<'_> = mutable.iter_mut();
        assert_eq!(iter.len(), 0);
        assert_eq!(iter.size_hint(), (0, Some(0)));
        assert!(iter.next().is_none());
        let mut values: super::ValuesMut<'_> = mutable.values_mut();
        assert_eq!(values.len(), 0);
        assert_eq!(values.size_hint(), (0, Some(0)));
        assert!(values.next().is_none());
        let mut iter: super::IterMut<'_> = (&mut mutable).into_iter();
        assert_eq!(iter.len(), 0);
        assert_eq!(iter.size_hint(), (0, Some(0)));
        assert!(iter.next().is_none());
        assert!(mutable.map.is_some());

        for empty in [object, mutable] {
            let mut iter: super::IntoIter = empty.into_iter();
            assert_eq!(iter.len(), 0);
            assert_eq!(iter.size_hint(), (0, Some(0)));
            assert!(iter.next().is_none());
        }
    }

    #[test]
    fn unique_clear_preserves_allocated_storage_and_capacity_for_reuse() {
        let mut object = sample();
        let storage = std::sync::Arc::as_ptr(object.map.as_ref().unwrap());
        let capacity = object.map.as_ref().unwrap().capacity();
        object.clear();
        assert!(object.is_empty());
        assert_eq!(
            std::sync::Arc::as_ptr(object.map.as_ref().unwrap()),
            storage
        );
        assert_eq!(object.map.as_ref().unwrap().capacity(), capacity);
        object.insert("new".into(), Value::scalar(9));
        assert_eq!(
            std::sync::Arc::as_ptr(object.map.as_ref().unwrap()),
            storage
        );
        assert_eq!(object.map.as_ref().unwrap().capacity(), capacity);
        assert_eq!(object, crate::object!({"new": 9}));
    }

    #[test]
    fn insert_and_remove_preserve_replaced_values_and_other_clones() {
        let original = sample();
        let mut copy = original.clone();
        assert_eq!(
            copy.insert("first".into(), Value::scalar(9)),
            Some(Value::scalar(1))
        );
        assert_eq!(copy.insert("third".into(), Value::scalar(3)), None);
        assert_eq!(copy.remove("second"), Some(Value::scalar(2)));
        assert_eq!(copy.remove("absent"), None);
        assert_eq!(original, sample());
        assert_eq!(copy, crate::object!({"first": 9, "third": 3}));
    }

    #[test]
    fn mutable_lookup_and_indexing_detach_before_exposing_values() {
        let original = compound();
        let mut copy = original.clone();
        change_item(copy.get_mut("items").unwrap());
        copy["nil"] = Value::scalar("present");
        assert_eq!(original, compound());
        assert_eq!(copy["nil"], Value::scalar("present"));
        assert_ne!(copy["items"], original["items"]);
    }

    #[test]
    fn clear_does_not_clear_a_clone_and_allows_reuse() {
        let original = compound();
        let mut copy = original.clone();
        copy.clear();
        assert!(copy.is_empty());
        assert_eq!(original, compound());
        copy.insert("new".into(), Value::scalar(7));
        assert_eq!(copy, crate::object!({"new": 7}));
    }

    #[test]
    fn occupied_entry_mutation_paths_leave_clones_unchanged() {
        let original = sample();
        for mutation in 0..4 {
            let mut copy = original.clone();
            let Entry::Occupied(mut entry) = copy.entry("first") else {
                panic!("Expected occupied entry");
            };
            assert_eq!(entry.key().as_str(), "first");
            assert_eq!(entry.get(), &Value::scalar(1));
            match mutation {
                0 => *entry.get_mut() = Value::scalar(9),
                1 => *entry.into_mut() = Value::scalar(9),
                2 => assert_eq!(entry.insert(Value::scalar(9)), Value::scalar(1)),
                3 => assert_eq!(entry.remove(), Value::scalar(1)),
                _ => unreachable!(),
            }
            assert_eq!(original, sample());
            if mutation == 3 {
                assert!(!copy.contains_key("first"));
            } else {
                assert_eq!(copy["first"], Value::scalar(9));
            }
        }
    }

    #[test]
    fn vacant_entry_returns_a_mutable_value_only_in_its_object() {
        let original = sample();
        let mut copy = original.clone();
        let Entry::Vacant(entry) = copy.entry("third") else {
            panic!("Expected vacant entry");
        };
        assert_eq!(entry.key().as_str(), "third");
        *entry.insert(Value::scalar(3)) = Value::scalar(9);
        assert_eq!(copy["third"], Value::scalar(9));
        assert_eq!(original, sample());
    }

    #[test]
    fn entry_defaults_preserve_existing_values_and_closure_contract() {
        let original = sample();
        let mut copy = original.clone();
        assert_eq!(copy.entry("first").key().as_str(), "first");
        *copy.entry("first").or_insert(Value::scalar(8)) = Value::scalar(9);
        assert_eq!(
            copy.entry("third").or_insert(Value::scalar(3)),
            &Value::scalar(3)
        );
        assert_eq!(
            copy.entry("second")
                .or_insert_with(|| panic!("Existing entry must not call default")),
            &Value::scalar(2)
        );
        let mut calls = 0;
        let value = copy.entry("fourth").or_insert_with(|| {
            calls += 1;
            Value::scalar(4)
        });
        *value = Value::scalar(8);
        assert_eq!(calls, 1);
        assert_eq!(copy["first"], Value::scalar(9));
        assert_eq!(copy["fourth"], Value::scalar(8));
        assert_eq!(original, sample());
    }

    #[test]
    fn all_mutable_iterators_detach_before_yielding() {
        let original = sample();
        let mut via_iter = original.clone();
        let mut iterator = via_iter.iter_mut();
        assert_eq!(iterator.len(), 2);
        assert_eq!(iterator.size_hint(), (2, Some(2)));
        for (_, value) in &mut iterator {
            *value = Value::scalar(9);
        }
        assert_eq!(iterator.len(), 0);

        let mut via_values = original.clone();
        for value in via_values.values_mut() {
            *value = Value::scalar(9);
        }
        let mut via_into = original.clone();
        for (_, value) in &mut via_into {
            *value = Value::scalar(9);
        }
        let expected = crate::object!({"first": 9, "second": 9});
        assert_eq!(via_iter, expected);
        assert_eq!(via_values, expected);
        assert_eq!(via_into, expected);
        assert_eq!(original, sample());
    }

    #[test]
    fn extend_replaces_and_adds_without_modifying_a_clone() {
        let original = sample();
        let mut copy = original.clone();
        copy.extend([
            ("first".into(), Value::scalar(9)),
            ("third".into(), Value::scalar(3)),
        ]);
        assert_eq!(copy, crate::object!({"first": 9, "second": 2, "third": 3}));
        assert_eq!(original, sample());
    }

    #[test]
    fn missing_mutable_lookups_and_removals_preserve_both_objects() {
        let original = compound();
        let mut copy = original.clone();
        assert!(copy.get_mut("missing").is_none());
        assert!(copy.remove("missing").is_none());
        assert_eq!(original, compound());
        assert_eq!(copy, original);
    }

    #[test]
    fn read_iterators_keep_item_types_and_exact_lengths() {
        let object = sample();
        let mut iter: super::Iter<'_> = (&object).into_iter();
        assert_eq!(iter.len(), 2);
        let (key, value): (&crate::model::KString, &Value) = iter.next().unwrap();
        assert_eq!(object.get(key), Some(value));
        assert_eq!(iter.size_hint(), (1, Some(1)));
        assert_eq!(object.keys().len(), 2);
        assert_eq!(object.values().len(), 2);
    }

    #[test]
    fn unique_owning_iteration_keeps_types_and_consumes_every_value() {
        let original = compound();
        let expected = serde_yaml::to_string(&original).unwrap();
        let mut iter: super::IntoIter = original.into_iter();
        assert_eq!(iter.len(), 3);
        let first: (crate::model::KString, Value) = iter.next().unwrap();
        assert_eq!(iter.size_hint(), (2, Some(2)));
        let recovered: Object = std::iter::once(first).chain(iter).collect();
        assert_eq!(
            recovered,
            serde_yaml::from_str::<Object>(&expected).unwrap()
        );
    }

    #[test]
    fn shared_owning_iteration_yields_independent_nested_values() {
        let original = compound();
        let mut recovered = Object::new();
        let iter: super::IntoIter = original.clone().into_iter();
        assert_eq!(iter.len(), 3);
        for (key, mut value) in iter {
            if key == "items" {
                change_item(&mut value);
            }
            recovered.insert(key, value);
        }
        assert_eq!(original, compound());
        assert_ne!(recovered["items"], original["items"]);
        assert_eq!(recovered["nested"], original["nested"]);
    }

    #[test]
    fn nested_objects_and_arrays_detach_at_each_mutated_level() {
        let original = crate::object!({"outer": compound()});
        let mut copy = original.clone();
        let Value::Object(outer) = copy.get_mut("outer").unwrap() else {
            panic!("Expected nested object");
        };
        change_item(outer.get_mut("items").unwrap());
        let Value::Object(nested) = outer.get_mut("nested").unwrap() else {
            panic!("Expected nested object");
        };
        nested["count"] = Value::scalar(9);
        assert_eq!(original, crate::object!({"outer": compound()}));
        assert_ne!(copy, original);
    }

    #[test]
    fn serde_and_value_states_remain_semantic_for_shared_objects() {
        let original = compound();
        let mut decoded: Object =
            serde_yaml::from_str(&serde_yaml::to_string(&original).unwrap()).unwrap();
        let snapshot = decoded.clone();
        decoded["nil"] = Value::scalar(false);
        assert_eq!(snapshot, original);
        assert_ne!(decoded, original);
        assert!(original.query_state(State::Truthy));
        assert!(!original.query_state(State::Empty));
        assert!(Object::new().query_state(State::Truthy));
        assert!(Object::new().query_state(State::Blank));
        assert!(Object::new().query_state(State::DefaultValue));
        assert_eq!(original.type_name(), "object");
    }

    #[test]
    fn assigned_result_survives_replacement_and_mutation_independently() {
        let globals = crate::object!({"source": compound()});
        let runtime = RuntimeBuilder::new().set_globals(&globals).build();
        runtime.set_global("saved".into(), globals["source"].clone());
        let retained = runtime.try_get(&["saved".into()]).unwrap();
        assert!(matches!(retained, ValueCow::Owned(_)));
        // Retaining the result must not retain a RefCell guard or prevent writes.
        let replaced = runtime
            .set_global("saved".into(), Value::scalar("replacement"))
            .unwrap();
        assert_eq!(retained, replaced);
        assert_eq!(
            runtime.try_get(&["saved".into()]).unwrap().to_kstr(),
            "replacement"
        );
        let Value::Object(mut old) = retained.into_owned() else {
            panic!("Expected retained assigned object");
        };
        change_item(old.get_mut("items").unwrap());
        assert_eq!(replaced, Value::Object(compound()));
        assert_eq!(globals["source"], Value::Object(compound()));
        assert_eq!(
            runtime.try_get(&["saved".into()]).unwrap().to_kstr(),
            "replacement"
        );
    }

    #[test]
    fn object_preserves_send_and_sync_contract() {
        fn require_send_sync<T: Send + Sync>() {}
        require_send_sync::<Object>();
        require_send_sync::<Value>();
    }
}
