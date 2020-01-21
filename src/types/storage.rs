use super::chunk_storage::*;
use super::entry::Entry;
use crate::internal::hasher::HasherImpl;
use crate::internal::mr::rvec::RVec;
use crate::traits::idxset::IdxSet;
use crate::traits::memory_usage::{MemoryUsage, MemoryUser};
use crate::traits::query::Query;
use crate::traits::record::Record;
use crate::traits::valid_key::{BorrowedKey, ValidKey};
use crate::types::editor::Editor;
use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Chunked, indexed storage.
///
/// # Type Parameters
///
/// * `ChunkKey`: each `Element` is a `Record` that has exactly one `ChunkKey`. All `Elements`
///   with the same value of `ChunkKey` are stored together in a single chunk. If you aren't
///   sure what `ChunkKey` to use, choose `()`.
/// * `ItemKey`: each `Element` is a `Record` that has exactly one `ItemKey`. Every `Element`
///   within a chunk must have an `ItemKey` that is unique to that chunk.
/// * `Element`: the type contained in this `Storage`.
#[derive(Clone)]
pub struct Storage<ChunkKey: ?Sized, ItemKey: ?Sized, Element>
where
    ChunkKey: BorrowedKey,
    ChunkKey::Owned: ValidKey,
    ItemKey: BorrowedKey,
    ItemKey::Owned: ValidKey,
{
    id: u64,
    chunks: RVec<ChunkStorage<ChunkKey, ItemKey, Element>>,
    dirty: Vec<usize>,
    index: HashMap<ChunkKey::Owned, usize, HasherImpl>,
}

impl<ChunkKey, ItemKey, Element> Storage<ChunkKey, ItemKey, Element>
where
    ChunkKey: BorrowedKey + ?Sized,
    ChunkKey::Owned: ValidKey,
    ItemKey: BorrowedKey + ?Sized,
    ItemKey::Owned: ValidKey,
    Element: Record<ChunkKey, ItemKey>,
{
    /// Construct a new Storage.
    ///
    /// # Example
    ///
    /// ```
    /// use retriever::prelude::*;
    ///
    /// // Note that (A,B,C) implements Record<A,B>.
    /// let mut storage : Storage<u64, &'static str, (u64,&'static str, String)> = Storage::new();
    ///
    /// // In a later example, we'll encourage jroberts to use a stronger password.
    /// let user_id = 7;
    /// let username = String::from("jroberts");
    /// let password = String::from("PASSWORD!5");
    /// let admin = String::from("true");
    ///
    /// // For this example we choose a storage that represents some account information for a
    /// // single user. The tuple (Key, Value) type has a built-in impl for Record.
    /// storage.add((user_id, "username", username.clone()));
    /// storage.add((user_id, "password", password.clone()));
    /// storage.add((user_id, "admin", admin.clone()));
    ///
    /// // We can lookup the value of the "admin" field using it's item key.
    /// let is_admin = storage.get(&ID.chunk(user_id).item("admin"));
    /// assert_eq!(is_admin, Some(&(7, "admin",admin.clone())));
    ///
    /// # storage.validate();
    /// ```
    pub fn new() -> Self {
        Storage {
            id: ID_COUNTER.fetch_add(1, Ordering::Relaxed),
            chunks: RVec::default(),
            dirty: Vec::default(),
            index: HashMap::with_hasher(crate::internal::hasher::HasherImpl::default()),
        }
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// Get the ChunkStorage corresponding the given ChunkKey.
    fn chunk(
        &mut self,
        chunk_key: &ChunkKey,
        dirty: bool,
    ) -> &mut ChunkStorage<ChunkKey, ItemKey, Element> {
        let idx = if let Some(idx) = self.internal_idx_of(chunk_key) {
            idx
        } else {
            let new_idx = self.chunks.len();
            self.index.insert(chunk_key.to_owned(), new_idx);
            self.chunks.push(ChunkStorage::new(chunk_key.to_owned()));
            new_idx
        };

        if dirty {
            self.dirty(idx);
        }

        &mut self.chunks[idx]
    }

    /// Add the given element to this Storage.
    ///
    /// # Example
    ///
    /// ```
    /// use retriever::prelude::*;
    /// use std::borrow::Cow;
    ///
    /// // This example will be a database of student records.
    /// struct Student {
    ///   school: String,
    ///   id: u64,
    ///   first_name: String,
    ///   last_name: String,
    /// }
    ///
    /// // Do note! Using the school name as the chunk key does mean that we'll have to
    /// // delete and re-add students who move to a different school.
    /// impl Record<String, u64> for Student {
    ///   fn chunk_key(&self) -> Cow<String> {
    ///     Cow::Borrowed(&self.school)
    ///   }
    ///
    ///   fn item_key(&self) -> Cow<u64> {
    ///     Cow::Owned(self.id)
    ///   }
    /// }
    ///
    /// let mut storage : Storage<String, u64, Student> = Storage::new();
    ///
    /// storage.add(Student {
    ///   school: String::from("PS109"),
    ///   id: 89875,
    ///   first_name: String::from("Mary"),
    ///   last_name: String::from("Jones"),
    /// });
    ///
    /// storage.add(Student {
    ///   school: String::from("PS109"),
    ///   id: 99200,
    ///   first_name: String::from("Alisha"),
    ///   last_name: String::from("Wu"),
    /// });
    ///
    /// storage.add(Student {
    ///   school: String::from("Northwood Elementary"),
    ///   id: 01029,
    ///   first_name: String::from("Anders"),
    ///   last_name: String::from("McAllister"),
    /// });
    ///
    /// let anders = storage.get(&ID.chunk(String::from("Northwood Elementary")).item(01029));
    /// assert_eq!(&anders.unwrap().first_name, "Anders");
    ///
    /// # storage.validate();
    /// ```
    pub fn add(&mut self, element: Element) -> &mut Self {
        self.clean();

        let chunk_key = element.chunk_key();
        let chunk_key_ref = chunk_key.borrow();
        self.chunk(chunk_key_ref, false).add(element);

        self
    }

    /// Add some elements that are all part of the same chunk.
    ///
    /// # Panic
    ///
    /// This method panics if any `Element` does not have the same chunk key as the others.
    ///
    pub fn add_chunk<I, K>(&mut self, i: I) -> &mut Self
    where
        I: IntoIterator<Item = K>,
        Element: Borrow<K>,
        K: ToOwned<Owned = Element> + Record<ChunkKey, ItemKey>,
    {
        self.clean();

        let mut i = i.into_iter().peekable();

        if let Some(chunk_key_cow) = i.peek().map(|x| x.chunk_key()) {
            self.chunk(chunk_key_cow.borrow(), false).extend(i);
        }

        self
    }

    /// Add many many elements, grouped into chunks.
    ///
    /// # Type Parameters
    ///
    /// * `II`: An iterator over groups of elements, each group belonging to a single chunk.
    /// * `I`: An iterator over some elements that all belong to the same chunk.
    /// * 'K': An `Element` or reference to an `Element`.
    ///
    /// # Panic
    ///
    /// This method panics if any group of `Elements` do not share a common chunk key, or any
    /// two groups of `Elements` do share a common chunk key.
    ///
    pub fn add_chunks<I, II, K>(&mut self, ii: II) -> &mut Self
    where
        II: IntoIterator<Item = I>,
        I: IntoIterator<Item = K>,
        Element: Borrow<K>,
        K: ToOwned<Owned = Element> + Record<ChunkKey, ItemKey>,
    {
        self.clean();

        for i in ii {
            self.add_chunk(i);
        }

        self
    }

    fn clean(&mut self) {
        if self.dirty.is_empty() {
            return;
        }

        self.dirty.sort_unstable();

        for idx in self.dirty.iter().rev() {
            if !self.chunks[*idx].is_empty() {
                continue;
            }

            self.index.remove(self.chunks[*idx].chunk_key());
            self.chunks.swap_remove(*idx);
            if self.chunks.len() > *idx {
                self.index
                    .insert(self.chunks[*idx].chunk_key().to_owned(), *idx);
            }
        }

        self.dirty.clear();
    }

    fn dirty(&mut self, idx: usize) {
        self.dirty.push(idx);
    }

    /// Dissolve this Storage into a list of chunks.
    pub fn dissolve(self) -> impl IntoIterator<Item = Vec<Element>> {
        let chunks: Vec<_> = self.chunks.into();
        chunks.into_iter().map(|chunk| chunk.into())
    }

    /// Raw serial access to all element data by reference.
    /// In many cases, you may prefer to use `Storage::iter()` to simply iterate every element.
    ///
    /// You can also use `Storage::dissolve()`, but this consumes the `Storage`.
    ///
    /// # Example
    ///
    /// ```
    /// use retriever::prelude::*;
    ///
    /// // Load some data into storage.
    /// let mut storage : Storage<usize, usize, (usize, usize, String)> = Storage::new();
    ///
    /// storage.add((109, 0, String::from("hello")));
    /// storage.add((109, 1, String::from("doctor")));
    /// storage.add((109, 2, String::from("name")));
    /// storage.add((9000, 3, String::from("continue")));
    /// storage.add((9000, 4, String::from("yesterday")));
    /// storage.add((9000, 5, String::from("tomorrow")));
    ///
    /// let for_serialization : Vec<&[(usize, usize, String)]> = storage.raw().collect();
    /// let serialized = serde_json::to_string(&for_serialization).unwrap();
    ///
    /// let deserialized : Vec<Vec<(usize, usize, String)>> = serde_json::from_str(&serialized).unwrap();
    /// let mut duplicated_storage : Storage<usize, usize, (usize, usize, String)> = Storage::new();
    /// duplicated_storage.add_chunks(deserialized);
    ///
    /// assert_eq!(Some(&(109, 0, String::from("hello"))), duplicated_storage.get(&ID.chunk(109).item(0)));
    /// assert_eq!(Some(&(109, 1, String::from("doctor"))), duplicated_storage.get(&ID.chunk(109).item(1)));
    /// assert_eq!(Some(&(109, 2, String::from("name"))), duplicated_storage.get(&ID.chunk(109).item(2)));
    /// assert_eq!(Some(&(9000, 3, String::from("continue"))), duplicated_storage.get(&ID.chunk(9000).item(3)));
    /// assert_eq!(Some(&(9000, 4, String::from("yesterday"))), duplicated_storage.get(&ID.chunk(9000).item(4)));
    /// assert_eq!(Some(&(9000, 5, String::from("tomorrow"))), duplicated_storage.get(&ID.chunk(9000).item(5)));
    ///
    /// # storage.validate();
    /// # duplicated_storage.validate();
    /// ```
    pub fn raw(&self) -> impl Iterator<Item = &[Element]> {
        self.chunks.iter().map(|chunk| chunk.raw())
    }

    /// Get an `Element`, if it exists. An `Element` is a `Record` that is uniquely identified
    /// by the combination of its `ChunkKey` and `ItemKey`.
    ///
    /// Returns None if the data element does not exist.
    ///
    /// # Type Parameters
    ///
    /// * `R`: Any `Record` with the same `ChunkKey` and `ItemKey` as the record you want to
    /// access. If there's no obvious choice for `R`, consider using `retriever::types::id::Id`
    /// to construct an appropriate key.
    ///
    /// # Example
    ///
    /// ```
    /// use retriever::prelude::*;
    /// use std::borrow::Cow;
    ///
    /// struct Song {
    ///   playlist_id: usize,
    ///   song_id: usize,
    ///   favorite: bool,
    /// }
    ///
    /// let mut storage : Storage<usize,usize,Song> = Storage::new();
    ///
    /// impl Record<usize,usize> for Song {
    ///   fn chunk_key(&self) -> Cow<usize> {
    ///     Cow::Owned(self.playlist_id)
    ///   }
    ///
    ///   fn item_key(&self) -> Cow<usize> {
    ///     Cow::Owned(self.song_id)
    ///   }
    /// }
    ///
    /// storage.add(Song {
    ///   playlist_id: 1,
    ///   song_id: 1,
    ///   favorite: false,
    /// });
    ///
    /// storage.add(Song {
    ///   playlist_id: 1,
    ///   song_id: 2,
    ///   favorite: true,
    /// });
    ///
    /// storage.add(Song {
    ///   playlist_id: 2,
    ///   song_id: 1,
    ///   favorite: false,
    /// });
    ///
    /// assert_eq!(Some(false), storage.get(&ID.chunk(1).item(1)).map(|song| song.favorite));
    /// assert_eq!(Some(true), storage.get(&ID.chunk(1).item(2)).map(|song| song.favorite));
    /// assert_eq!(Some(false), storage.get(&ID.chunk(2).item(1)).map(|song| song.favorite));
    ///
    /// # storage.validate();
    /// ```
    pub fn get<R>(&self, unique_id: &R) -> Option<&Element>
    where
        R: Record<ChunkKey, ItemKey>,
    {
        self.internal_idx_of(unique_id.borrow().chunk_key().borrow())
            .and_then(|idx| self.chunks[idx].get(unique_id))
    }

    /// Get an `Entry` for an `Element` that may or may not exist. An `Element` is a `Record`
    /// that is uniquely identified by the combination of its `ChunkKey` and `ItemKey`.
    ///
    /// This Entry API is very similar to the Entry APIs provided by rust's
    /// standard collections API.
    ///
    /// # Type Parameters:
    ///
    /// * `R`: Any `Record` with the same `ChunkKey` and `ItemKey` as the record you want to
    /// access. If there's no obvious choice for `R`, consider using `retriever::types::id::Id`
    /// to construct an appropriate key.
    ///
    /// # Example
    ///
    /// ```
    /// use retriever::prelude::*;
    /// use std::borrow::Cow;
    ///
    /// struct Song {
    ///   playlist_id: usize,
    ///   song_id: usize,
    ///   favorite: bool,
    /// }
    ///
    /// let mut storage : Storage<usize,usize,Song> = Storage::new();
    ///
    /// impl Record<usize,usize> for Song {
    ///   fn chunk_key(&self) -> Cow<usize> {
    ///     Cow::Owned(self.playlist_id)
    ///   }
    ///
    ///   fn item_key(&self) -> Cow<usize> {
    ///     Cow::Owned(self.song_id)
    ///   }
    /// }
    ///
    /// storage.add(Song {
    ///   playlist_id: 1,
    ///   song_id: 1,
    ///   favorite: false,
    /// });
    ///
    /// storage.add(Song {
    ///   playlist_id: 1,
    ///   song_id: 2,
    ///   favorite: true,
    /// });
    ///
    /// storage.add(Song {
    ///   playlist_id: 2,
    ///   song_id: 1,
    ///   favorite: false,
    /// });
    ///
    /// // Entry::get() does the same thing as Storage::get()
    /// assert_eq!(Some(false), storage.entry(&ID.chunk(1).item(1)).get().map(|song| song.favorite));
    ///
    /// // Entry::get_mut() supports mutation
    /// if let Some(song) = storage.entry(&ID.chunk(1).item(2)).get_mut() {
    ///   song.favorite = false;
    /// }
    /// assert_eq!(Some(false), storage.get(&ID.chunk(1).item(2)).map(|song| song.favorite));
    ///
    /// // Entry::and_modify() also supports mutation; does nothing if the item does not exist.
    /// storage.entry(&ID.chunk(2).item(1)).and_modify(|song| {
    ///   song.favorite = true;
    /// });
    /// assert_eq!(Some(true), storage.get(&ID.chunk(2).item(1)).map(|song| song.favorite));
    ///
    /// // Entry::or_insert_with() is another way to mutate,
    /// // in this case inserting if the item does not exist.
    /// let mut song = storage.entry(ID.chunk(3).item(1)).or_insert_with(|| Song {
    ///   playlist_id: 3,
    ///   song_id: 1,
    ///   favorite: false,
    /// });
    /// song.favorite = true;
    /// assert_eq!(Some(true), storage.get(&ID.chunk(3).item(1)).map(|song| song.favorite));
    ///
    /// # storage.validate();
    /// ```
    pub fn entry<'a, R>(&'a mut self, unique_id: R) -> Entry<'a, R, ChunkKey, ItemKey, Element>
    where
        R: Record<ChunkKey, ItemKey> + 'a,
    {
        self.clean();
        self.chunk(unique_id.borrow().chunk_key().borrow(), true)
            .entry(unique_id)
    }

    /// Iterate over every element in storage.
    ///
    /// # Example
    ///
    /// ```
    /// use retriever::prelude::*;
    ///
    /// // Note that (A,B,C) implements Record<A,B>.
    /// let mut storage : Storage<usize,usize,(usize,usize,i64)> = Storage::new();
    ///
    /// storage.add((1,1000,17));
    /// storage.add((1,1001,53));
    /// storage.add((1,1002,-57));
    /// storage.add((2,2000,29));
    /// storage.add((2,2001,-19));
    /// storage.add((3,3002,-23));
    ///
    /// // All elements together should sum to zero:
    /// assert_eq!(0, storage.iter().map(|x| x.2).sum::<i64>());
    ///
    /// # storage.validate();
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = &Element> {
        self.chunks.iter().flat_map(|chunk| chunk.iter())
    }

    /// Iterate over elements according to some Query. A variety of builtin queries are provided.
    ///
    /// # Type Parameters
    ///
    /// * `Q`: Any `Query`. There are a variety of useful `Queries`:
    ///   * `Everything`
    ///   * `Chunks(...)`, an explicit list of chunks
    ///   * `Id`, the Id of a specific element
    ///   * Most other `Queries` can be constructed by chaining the methods of the `Query` trait.
    ///
    /// # Example
    ///
    /// ```
    /// use retriever::prelude::*;
    /// use std::borrow::Cow;
    ///
    /// // Note that (A,B,C) implements Record<A,B>.
    /// let mut storage : Storage<u8,u16,(u8,u16,i64)> = Storage::new();
    ///
    /// storage.add((1,1000,17));
    /// storage.add((1,1001,53));
    /// storage.add((1,1002,-57));
    /// storage.add((2,2000,29));
    /// storage.add((2,2001,-19));
    /// storage.add((3,3002,-23));
    ///
    /// // All of these do the same thing:
    /// assert_eq!(0, storage.query(Everything).map(|x| x.2).sum::<i64>());
    /// assert_eq!(0, storage.query(&Everything).map(|x| x.2).sum::<i64>());
    /// let chunk_ids : &[u8] = &[0,1,2,3];
    /// assert_eq!(0, storage.query(Chunks(chunk_ids)).map(|x| x.2).sum::<i64>());
    /// assert_eq!(0, storage.query(&Chunks([0,1,2,3])).map(|x| x.2).sum::<i64>());
    /// assert_eq!(0, storage.query(&Chunks(vec![0,1,2,3])).map(|x| x.2).sum::<i64>());
    ///
    /// // Query only a specific item:
    /// assert_eq!(53, storage.query(ID.chunk(1).item(1001)).map(|x| x.2).sum::<i64>());
    ///
    /// // You can also filter to only look at positive numbers:
    /// assert_eq!(99, storage.query(Everything.filter(|x : &(u8,u16,i64)| x.2 > 0)).map(|x| x.2).sum::<i64>());
    ///
    /// // Or accelerate the exact same filter using a SecondaryIndex:
    /// let mut positive_numbers : SecondaryIndex<u8,(u8,u16,i64),Option<bool>,bool> =
    ///     SecondaryIndex::new(&storage, |x : &(u8,u16,i64)| Cow::Owned(Some(x.2 > 0)));
    /// assert_eq!(99, storage.query(&Everything.matching(&mut positive_numbers, Cow::Owned(true))).map(|x| x.2).sum::<i64>());
    ///
    /// # storage.validate();
    /// ```
    pub fn query<'a, Q>(&'a self, query: Q) -> impl Iterator<Item = &'a Element>
    where
        Q: Query<ChunkKey, ItemKey, Element> + Clone + 'a,
    {
        let chunk_idxs = query.chunk_idxs(&self);

        chunk_idxs
            .into_idx_iter()
            .flatten()
            .map(move |idx| &self.chunks[idx])
            .flat_map(
                move |chunk_storage: &ChunkStorage<ChunkKey, ItemKey, Element>| {
                    chunk_storage.query(query.clone())
                },
            )
    }

    /// Iterate over a Query and modify each element via a callback.
    /// The callback provides retriever's Editor API, which in turn provides
    /// a mutable or immutable reference to the underlying element.
    ///
    /// Since re-indexing is a potentially expensive operation, it's best to examine an immutable
    /// reference to a data element to make sure you really want to mutate it before obtaining a
    /// mutable reference.
    ///
    /// # Type Parameters
    ///
    /// * `Q`: Any `Query`. There are a variety of useful `Queries`:
    ///   * `Everything`
    ///   * `Chunks(...)`, an explicit list of chunks
    ///   * `Id`, the Id of a specific element
    ///   * Most other `Queries` can be constructed by chaining the methods of the `Query` trait.
    ///
    /// ```
    /// use retriever::prelude::*;
    /// use std::borrow::Cow;
    ///
    /// struct BankAccount {
    ///   id: usize,
    ///   balance: i64,
    /// }
    ///
    /// impl Record<(),usize> for BankAccount {
    ///   fn chunk_key(&self) -> Cow<()> {
    ///     Cow::Owned(())
    ///   }
    ///
    ///   fn item_key(&self) -> Cow<usize> {
    ///     Cow::Owned(self.id)
    ///   }
    /// }
    ///
    /// let mut storage : Storage<(),usize,BankAccount> = Storage::new();
    ///
    /// storage.add(BankAccount { id: 1, balance: 25 });
    /// storage.add(BankAccount { id: 2, balance: 13 });
    /// storage.add(BankAccount { id: 3, balance: -900 });
    /// storage.add(BankAccount { id: 4, balance: 27000 });
    /// storage.add(BankAccount { id: 5, balance: -13 });
    ///
    /// // Charge an overdraft fee to everyone with a negative balance.
    /// storage.modify(Everything.filter(|account : &BankAccount| account.balance < 0), |mut account| {
    ///   account.get_mut().balance -= 25;
    /// });
    ///
    /// assert_eq!(Some(25),    storage.get(&ID.item(1)).map(|x| x.balance));
    /// assert_eq!(Some(13),    storage.get(&ID.item(2)).map(|x| x.balance));
    /// assert_eq!(Some(-925),  storage.get(&ID.item(3)).map(|x| x.balance));
    /// assert_eq!(Some(27000), storage.get(&ID.item(4)).map(|x| x.balance));
    /// assert_eq!(Some(-38),   storage.get(&ID.item(5)).map(|x| x.balance));
    ///
    /// # storage.validate();
    /// ```
    pub fn modify<Q, F>(&mut self, query: Q, f: F)
    where
        Q: Query<ChunkKey, ItemKey, Element>,
        F: Fn(Editor<ChunkKey, ItemKey, Element>),
    {
        self.clean();

        for idx in query.chunk_idxs(self).into_idx_iter().flatten() {
            self.chunks[idx].modify(&query, &f);
        }
    }

    /// Remove all of the specified elements from this storage.
    ///
    /// # Type Parameters
    ///
    /// * `Q`: Any `Query`. There are a variety of useful `Queries`:
    ///   * `Everything`
    ///   * `Chunks(...)`, an explicit list of chunks
    ///   * `Id`, the Id of a specific element
    ///   * Most other `Queries` can be constructed by chaining the methods of the `Query` trait.
    ///
    /// ```
    /// use retriever::prelude::*;
    /// use retriever::queries::everything::Everything;
    /// use std::borrow::Cow;
    ///
    /// // In this example, we will store log entries, some of which might contain sensitive
    /// // information that we need to delete later.
    /// struct LogEntry {
    ///   stardate: u128,
    ///   msg: String,
    /// }
    ///
    /// impl Record<u128, u128> for LogEntry {
    ///   fn chunk_key(&self) -> Cow<u128> {
    ///     Cow::Owned(self.stardate / 1000)
    ///   }
    ///
    ///   fn item_key(&self) -> Cow<u128> {
    ///     Cow::Borrowed(&self.stardate)
    ///   }
    /// }
    ///
    /// let mut storage : Storage<u128, u128, LogEntry> = Storage::new();
    ///
    /// storage.add(LogEntry {
    ///   stardate: 109301,
    ///   msg: String::from("Departed from Starbase Alpha"),
    /// });
    ///
    /// storage.add(LogEntry {
    ///   stardate: 109302,
    ///   msg: String::from("Purchased illegal cloaking device from aliens"),
    /// });
    ///
    /// storage.add(LogEntry {
    ///   stardate: 109303,
    ///   msg: String::from("Asked doctor to check cat for space fleas"),
    /// });
    ///
    /// // Use the 'remove' operation to search for any embarassing log entries
    /// // and drop them.
    /// storage.remove(&Everything.filter(|log_entry: &LogEntry| {
    ///   log_entry.msg.contains("illegal")
    /// }), std::mem::drop);
    ///
    /// assert!(
    ///   storage
    ///     .get(&ID.chunk(109).item(109302))
    ///     .is_none());
    ///
    /// assert_eq!(
    ///   storage.iter().count(),
    ///   2);
    ///
    /// # storage.validate();
    /// ```
    pub fn remove<Q, F>(&mut self, query: Q, f: F)
    where
        F: Fn(Element),
        Q: Query<ChunkKey, ItemKey, Element>,
    {
        for idx in query.chunk_idxs(self).into_idx_iter().flatten() {
            self.dirty(idx);
            self.chunks[idx].remove(&query, &f);
        }

        self.clean();
    }

    /// List all chunks
    pub fn chunk_keys(&self) -> impl IntoIterator<Item = &ChunkKey> {
        self.chunks.iter().map(|chunk| chunk.chunk_key())
    }

    /// Drop an entire chunk and return all associated elements
    pub fn remove_chunk(&mut self, chunk_key: &ChunkKey) -> Option<Vec<Element>> {
        self.clean();
        let idx = self.index.remove(chunk_key)?;
        let chunk = self.chunks.swap_remove(idx);
        Some(chunk.into())
    }

    /// Panic if this storage is malformed or broken in any way.
    /// This is a slow operation and you shouldn't use it unless you suspect a problem.
    pub fn validate(&mut self) {
        self.clean();

        for (idx, chunk) in self.chunks.iter().enumerate() {
            assert_eq!(
                self.index.get(chunk.chunk_key()),
                Some(&idx),
                "chunk not indexed"
            );
        }

        for (chunk_key, idx) in self.index.iter() {
            assert_eq!(
                self.chunks[*idx].chunk_key(),
                chunk_key.borrow(),
                "index broken"
            );
            assert_ne!(self.chunks[*idx].len(), 0, "empty chunk");
        }

        for chunk in self.chunks.iter() {
            chunk.validate();
        }
    }

    pub(crate) fn internal_idx_of<Q>(&self, chunk_key: &Q) -> Option<usize>
    where
        Q: Eq + Hash + ToOwned<Owned = ChunkKey::Owned> + ?Sized,
        ChunkKey::Owned: Borrow<Q>,
    {
        self.index.get(chunk_key).cloned()
    }

    pub(crate) fn internal_rvec(&self) -> &RVec<ChunkStorage<ChunkKey, ItemKey, Element>> {
        &self.chunks
    }

    /// This method provides garbage collection services for the caller. Assuming that the
    /// `data` parameter is a HashMap that represents some data about chunks in this `Storage`,
    /// this method deletes all of the entries in that `HashMap` that no longer exist this `Storage`.
    ///
    /// The `chunk_list` parameter is an `RVec` containing all chunk keys. It is created for the
    /// purpose of being managed by this method and is managed entirely and only by this method.
    pub(crate) fn gc<T>(
        &self,
        chunk_list: &mut RVec<Option<ChunkKey::Owned>>,
        data: &mut HashMap<ChunkKey::Owned, T, crate::internal::hasher::HasherImpl>,
    ) {
        let mut removed: HashSet<ChunkKey::Owned, _> =
            HashSet::with_hasher(crate::internal::hasher::HasherImpl::default());
        let mut added: HashSet<ChunkKey::Owned, _> =
            HashSet::with_hasher(crate::internal::hasher::HasherImpl::default());

        chunk_list.reduce(&self.chunks, 1, |chunk_storages, prev_chunk_key, _| {
            if chunk_storages.is_empty() {
                if let Some(chunk_key) = prev_chunk_key.as_ref() {
                    removed.insert(chunk_key.clone());
                }
                None
            } else if Some(chunk_storages[0].chunk_key())
                != prev_chunk_key.as_ref().map(Borrow::borrow)
            {
                added.insert(chunk_storages[0].chunk_key().to_owned());
                if let Some(chunk_key) = prev_chunk_key.as_ref() {
                    removed.insert(chunk_key.clone());
                }
                Some(Some(chunk_storages[0].chunk_key().to_owned()))
            } else {
                None
            }
        });

        for chunk_key in removed.difference(&added) {
            data.remove(chunk_key.borrow());
        }
    }
}

impl<ChunkKey, ItemKey, Element> Default for Storage<ChunkKey, ItemKey, Element>
where
    ChunkKey: ValidKey,
    ItemKey: ValidKey,
    Element: Record<ChunkKey, ItemKey>,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<ChunkKey, ItemKey, Element> MemoryUser for Storage<ChunkKey, ItemKey, Element>
where
    ChunkKey: BorrowedKey + ?Sized,
    ChunkKey::Owned: ValidKey,
    ItemKey: BorrowedKey + ?Sized,
    ItemKey::Owned: ValidKey,
{
    fn memory_usage(&self) -> MemoryUsage {
        let mut result = MemoryUsage {
            size_of: None,
            len: 0,
            capacity: 0,
        };

        result = MemoryUsage::merge(result, self.index.memory_usage());
        result = MemoryUsage::merge(result, self.chunks.memory_usage());

        for chunk in self.chunks.iter() {
            result = MemoryUsage::merge(result, chunk.memory_usage());
        }

        result
    }

    fn shrink_with<F: Fn(&MemoryUsage) -> Option<usize>>(&mut self, f: F) {
        for i in 0..self.chunks.len() {
            if let Some(_min_capacity) = f(&self.chunks[i].memory_usage()) {
                self.chunks[i].shrink_with(&f);
            }
        }

        self.index.shrink_with(&f);
        self.chunks.shrink_with(&f);
    }
}
