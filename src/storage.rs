//! Storage layer — B+ tree, pager, and page types.
//!
//! All tables share a single database file via a shared `Rc<RefCell<Pager>>`.

mod btree;
mod page;
mod pager;

pub(crate) use btree::BPlusTree;

/// Alias for `Vec<u8>` — used throughout storage as the key/value byte container.
type Bytes = Vec<u8>;

/// Magic number: "SAKU" in ASCII
const MAGIC: u32 = 0x53414B55;

/// The size of each page in bytes.
const PAGE_SIZE: usize = 4096;
/// The maximum number of keys in a leaf page.
const MAX_KEYS_LEAF: usize = 100;
/// The maximum number of keys in an internal page.
const MAX_KEYS_INTERNAL: usize = 200;
/// The minimum number of keys in a leaf page.
const MIN_KEYS_LEAF: usize = MAX_KEYS_LEAF / 2;
/// The minimum number of keys in an internal page.
const MIN_KEYS_INTERNAL: usize = MAX_KEYS_INTERNAL / 2;
