//! Page types — header, internal, leaf — with serialization to 4096-byte arrays.

use anyhow::{Result, bail};

use super::{Bytes, MAGIC, PAGE_SIZE};

/// Header Page
///
/// ```text
/// [0, 4]  - magic number (0x53414B55 for "SAKU")
/// [4, 8]  - version number (currently 1)
/// [8, 12] - total number of pages in the file
/// [12, 16] - free list head page ID (0 if no free pages)
/// ```
///
/// The rest of the page is reserved and should be zeroed.
pub(super) struct HeaderPage {
    /// The magic number.
    pub(super) magic: u32,
    /// The version number of the page format.
    pub(super) version: u32,
    /// The total number of pages in the file, including the header page.
    pub(super) num_pages: u32,
    /// The page ID of the head of the free list, or 0 if there are no free pages.
    pub(super) free_list: u32,
}

/// Internal Page
///
/// ```text
/// [0] - page type byte (0x01 for internal)
/// [1, 3] - number of keys (u16)
/// [3, ...] - serialized keys and child page IDs:
///     For each key:
///         [4] - key length (u32)
///         [4 + key length] - key data
///     After all keys:
///     For each child page ID:
///         [4] - child page ID (u32)
/// ```
/// The page can hold up to `MAX_KEYS_INTERNAL` keys and `MAX_KEYS_INTERNAL` + 1 children.
pub(super) struct InternalPage {
    /// The keys stored in the internal page,
    /// which is the minimum key in the corresponding child page.
    pub(super) keys: Vec<Bytes>,
    /// The child page IDs corresponding to the keys.
    pub(super) children: Vec<u32>,
}

/// Leaf Page, store key-value pairs.
///
/// ```text
/// [0]   - page type byte (0x02 for leaf)
/// [1, 3]  - number of keys (u16)
/// [3, 7]  - next leaf page ID (u32, 0 if this is the last leaf)
/// [7, ...] - serialized key-value pairs:
///     For each key-value pair:
///         [4] - key length (u32)
///         [4 + key length] - key data
///         [4] - value length (u32)
///         [4 + value length] - value data
/// ```
///
/// The page can hold up to `MAX_KEYS_LEAF` key-value pairs.
pub(super) struct LeafPage {
    /// The keys stored in the leaf page.
    pub(super) keys: Vec<Bytes>,
    /// The values corresponding to the keys in the leaf page.
    ///
    /// This is the real data.
    pub(super) values: Vec<Bytes>,
    /// The page ID of the next leaf page, or 0 if this is the last leaf page.
    pub(super) next: u32,
}

/// A B+ tree page, which can be a header, internal node, or leaf node.
pub(super) enum Page {
    /// The header page, which contains metadata about the file and free list.
    Header(HeaderPage),
    /// An internal page, which contains keys and child page IDs for navigating the B+ tree.
    Internal(InternalPage),
    /// A leaf page, which contains key-value pairs and a pointer to the next leaf page.
    Leaf(LeafPage),
}

impl Page {
    /// Serialize the page into a 4096-byte array.
    ///
    /// # Returns
    ///
    /// * `Result<[u8; PAGE_SIZE]>` - On success, returns a 4096-byte array containing the serialized page data.
    pub(super) fn serialize(&self) -> Result<[u8; PAGE_SIZE]> {
        match self {
            Page::Header(h) => serialize_header(h),
            Page::Internal(p) => serialize_internal(p),
            Page::Leaf(p) => serialize_leaf(p),
        }
    }

    /// Deserialize a page from a 4096-byte array.
    ///
    /// # Arguments
    ///
    /// * `data` - A reference to a 4096-byte array containing the serialized page data.
    ///
    /// # Returns
    ///
    /// * `Result<Self>` - On success, returns the deserialized `Page` enum variant.
    pub(super) fn deserialize(data: &[u8; PAGE_SIZE]) -> Result<Self> {
        if data[0..4] == MAGIC.to_le_bytes() {
            deserialize_header(data).map(Page::Header)
        } else if data[0] == 0x01 {
            deserialize_internal(data).map(Page::Internal)
        } else if data[0] == 0x02 {
            deserialize_leaf(data).map(Page::Leaf)
        } else {
            bail!("unknown page type byte: {:#x}", data[0]);
        }
    }
}

/// Serialize a header page into a `PAGE_SIZE` byte array.
///
/// # Arguments
///
/// * `header` — the header page to serialize.
///
/// # Returns
///
/// * `Result<[u8; PAGE_SIZE]>` — the serialized byte array.
fn serialize_header(header: &HeaderPage) -> Result<[u8; PAGE_SIZE]> {
    let mut data = [0u8; PAGE_SIZE];
    data[0..4].copy_from_slice(&header.magic.to_le_bytes());
    data[4..8].copy_from_slice(&header.version.to_le_bytes());
    data[8..12].copy_from_slice(&header.num_pages.to_le_bytes());
    data[12..16].copy_from_slice(&header.free_list.to_le_bytes());

    Ok(data)
}

/// Deserialize a `HeaderPage` from a `PAGE_SIZE` byte array.
///
/// # Arguments
///
/// * `data` — the byte array to deserialize from.
///
/// # Returns
///
/// * `Result<HeaderPage>` — the deserialized header page.
fn deserialize_header(data: &[u8; PAGE_SIZE]) -> Result<HeaderPage> {
    let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
    if magic != MAGIC {
        bail!("invalid magic: {:#x}", magic);
    }

    Ok(HeaderPage {
        magic,
        version: u32::from_le_bytes(data[4..8].try_into().unwrap()),
        num_pages: u32::from_le_bytes(data[8..12].try_into().unwrap()),
        free_list: u32::from_le_bytes(data[12..16].try_into().unwrap()),
    })
}

/// Serialize an internal page into a `PAGE_SIZE` byte array.
///
/// # Arguments
///
/// * `page` — the internal page to serialize.
///
/// # Returns
///
/// * `Result<[u8; PAGE_SIZE]>` — the serialized byte array.
fn serialize_internal(page: &InternalPage) -> Result<[u8; PAGE_SIZE]> {
    let mut data = [0u8; PAGE_SIZE];
    data[0] = 0x01;
    let n = page.keys.len();
    data[1..3].copy_from_slice(&(n as u16).to_le_bytes());

    let mut offset: usize = 3;
    for key in &page.keys {
        if offset + 4 > PAGE_SIZE {
            bail!("internal page overflow at key length serialization");
        }
        let key_len = key.len() as u32;
        data[offset..offset + 4].copy_from_slice(&key_len.to_le_bytes());
        offset += 4;

        if offset + key.len() > PAGE_SIZE {
            bail!("internal page overflow at key serialization");
        }
        data[offset..offset + key.len()].copy_from_slice(key);
        offset += key.len();
    }

    for child in &page.children {
        if offset + 4 > PAGE_SIZE {
            bail!("internal page overflow at child serialization");
        }
        data[offset..offset + 4].copy_from_slice(&child.to_le_bytes());
        offset += 4;
    }

    Ok(data)
}

/// Deserialize an `InternalPage` from a `PAGE_SIZE` byte array.
///
/// # Arguments
///
/// * `data` — the byte array to deserialize from.
///
/// # Returns
///
/// * `Result<InternalPage>` — the deserialized internal page.
fn deserialize_internal(data: &[u8; PAGE_SIZE]) -> Result<InternalPage> {
    let n = u16::from_le_bytes(data[1..3].try_into().unwrap()) as usize;
    let mut keys = Vec::with_capacity(n);
    let mut offset: usize = 3;

    for _ in 0..n {
        if offset + 4 > PAGE_SIZE {
            bail!("internal page truncated at key length");
        }
        let key_len = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;

        if offset + key_len > PAGE_SIZE {
            bail!("internal page truncated at key data");
        }
        keys.push(data[offset..offset + key_len].to_vec());
        offset += key_len;
    }

    let mut children = Vec::with_capacity(n + 1);
    for _ in 0..=n {
        if offset + 4 > PAGE_SIZE {
            bail!("internal page truncated at child");
        }
        children.push(u32::from_le_bytes(
            data[offset..offset + 4].try_into().unwrap(),
        ));
        offset += 4;
    }

    Ok(InternalPage { keys, children })
}

/// Serialize a leaf page into a `PAGE_SIZE` byte array.
///
/// # Arguments
///
/// * `page` — the leaf page to serialize.
///
/// # Returns
///
/// * `Result<[u8; PAGE_SIZE]>` — the serialized byte array.
fn serialize_leaf(page: &LeafPage) -> Result<[u8; PAGE_SIZE]> {
    let mut data = [0u8; PAGE_SIZE];
    data[0] = 0x02;
    let n = page.keys.len();
    data[1..3].copy_from_slice(&(n as u16).to_le_bytes());
    data[3..7].copy_from_slice(&page.next.to_le_bytes());

    let mut offset: usize = 7;
    for i in 0..n {
        let key = &page.keys[i];
        let val = &page.values[i];

        if offset + 4 > PAGE_SIZE {
            bail!("leaf page overflow at key length serialization");
        }
        let key_len = key.len() as u32;
        data[offset..offset + 4].copy_from_slice(&key_len.to_le_bytes());
        offset += 4;

        if offset + key.len() > PAGE_SIZE {
            bail!("leaf page overflow at key serialization");
        }
        data[offset..offset + key.len()].copy_from_slice(key);
        offset += key.len();

        if offset + 4 > PAGE_SIZE {
            bail!("leaf page overflow at value length serialization");
        }
        let val_len = val.len() as u32;
        data[offset..offset + 4].copy_from_slice(&val_len.to_le_bytes());
        offset += 4;

        if offset + val.len() > PAGE_SIZE {
            bail!("leaf page overflow at value serialization");
        }
        data[offset..offset + val.len()].copy_from_slice(val);
        offset += val.len();
    }

    Ok(data)
}

/// Deserialize a `LeafPage` from a `PAGE_SIZE` byte array.
///
/// # Arguments
///
/// * `data` — the byte array to deserialize from.
///
/// # Returns
///
/// * `Result<LeafPage>` — the deserialized leaf page.
fn deserialize_leaf(data: &[u8; PAGE_SIZE]) -> Result<LeafPage> {
    let n = u16::from_le_bytes(data[1..3].try_into().unwrap()) as usize;
    let next = u32::from_le_bytes(data[3..7].try_into().unwrap());

    let mut keys = Vec::with_capacity(n);
    let mut values = Vec::with_capacity(n);
    let mut offset: usize = 7;

    for _ in 0..n {
        if offset + 4 > PAGE_SIZE {
            bail!("leaf page truncated at key length");
        }
        let key_len = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;

        if offset + key_len > PAGE_SIZE {
            bail!("leaf page truncated at key data");
        }
        keys.push(data[offset..offset + key_len].to_vec());
        offset += key_len;

        if offset + 4 > PAGE_SIZE {
            bail!("leaf page truncated at value length");
        }
        let val_len = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;

        if offset + val_len > PAGE_SIZE {
            bail!("leaf page truncated at value data");
        }
        values.push(data[offset..offset + val_len].to_vec());
        offset += val_len;
    }

    Ok(LeafPage { keys, values, next })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_roundtrip() {
        let header = HeaderPage {
            magic: MAGIC,
            version: 1,
            num_pages: 2,
            free_list: 0,
        };
        let data = Page::Header(header).serialize().unwrap();
        let page = Page::deserialize(&data).unwrap();
        match page {
            Page::Header(h) => {
                assert_eq!(h.magic, MAGIC);
                assert_eq!(h.version, 1);
                assert_eq!(h.num_pages, 2);
                assert_eq!(h.free_list, 0);
            }
            _ => panic!("expected Header"),
        }
    }

    #[test]
    fn test_internal_roundtrip() {
        let internal = InternalPage {
            keys: vec![b"abc".to_vec(), b"def".to_vec()],
            children: vec![10, 20, 30],
        };
        let data = Page::Internal(internal).serialize().unwrap();
        let page = Page::deserialize(&data).unwrap();
        match page {
            Page::Internal(p) => {
                assert_eq!(p.keys, vec![b"abc".to_vec(), b"def".to_vec()]);
                assert_eq!(p.children, vec![10, 20, 30]);
            }
            _ => panic!("expected Internal"),
        }
    }

    #[test]
    fn test_leaf_roundtrip() {
        let leaf = LeafPage {
            keys: vec![b"k1".to_vec(), b"k2".to_vec()],
            values: vec![b"v1".to_vec(), b"v2".to_vec()],
            next: 42,
        };
        let data = Page::Leaf(leaf).serialize().unwrap();
        let page = Page::deserialize(&data).unwrap();
        match page {
            Page::Leaf(p) => {
                assert_eq!(p.keys, vec![b"k1".to_vec(), b"k2".to_vec()]);
                assert_eq!(p.values, vec![b"v1".to_vec(), b"v2".to_vec()]);
                assert_eq!(p.next, 42);
            }
            _ => panic!("expected Leaf"),
        }
    }

    #[test]
    fn test_all_pages_are_4096_bytes() {
        let header = HeaderPage {
            magic: MAGIC,
            version: 1,
            num_pages: 0,
            free_list: 0,
        };
        let internal = InternalPage {
            keys: vec![],
            children: vec![0],
        };
        let leaf = LeafPage {
            keys: vec![],
            values: vec![],
            next: 0,
        };

        assert_eq!(Page::Header(header).serialize().unwrap().len(), PAGE_SIZE);
        assert_eq!(
            Page::Internal(internal).serialize().unwrap().len(),
            PAGE_SIZE
        );
        assert_eq!(Page::Leaf(leaf).serialize().unwrap().len(), PAGE_SIZE);
    }

    #[test]
    fn test_unknown_page_type() {
        let mut data = [0u8; PAGE_SIZE];
        data[0] = 0xFF;
        assert!(Page::deserialize(&data).is_err());
    }
}
