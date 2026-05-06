//! Disk-backed page manager with an LRU cache and free-list support.

use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

use anyhow::Result;

use super::{
    MAGIC, PAGE_SIZE,
    page::{HeaderPage, LeafPage, Page},
};

/// Maximum number of pages held in the LRU cache.
const MAX_CACHE: usize = 64;

/// Disk-backed page manager with an LRU cache.
///
/// Handles reading, writing, allocating, and freeing fixed-size pages.
pub(super) struct Pager {
    /// The underlying file for storing pages.
    file: File,

    // --- cache ---
    /// In-memory cache of recently accessed pages, keyed by page ID.
    cache: HashMap<u32, Page>,
    /// Set of page IDs that have been modified and need to be flushed to disk.
    order: Vec<u32>,

    // -- dirty ---
    /// Set of page IDs that have been modified and need to be flushed to disk.
    dirty: HashSet<u32>,

    /// Total number of pages in the file (including header and root).
    num_pages: u32,
    /// Page ID of the head of the free list (0 if none).
    free_list: u32,
}

impl Pager {
    /// Open (or create) a database file at the given path.
    ///
    /// # Arguments
    ///
    /// * `path` — path to the database file.
    ///
    /// # Returns
    ///
    /// * `Result<Self>` — a new or opened Pager instance.
    pub(super) fn open(path: &str) -> Result<Self> {
        let exists = Path::new(path).exists();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;

        if !exists {
            let header = HeaderPage {
                magic: MAGIC,
                version: 1,
                num_pages: 2,
                free_list: 0,
            };
            let root_leaf = LeafPage {
                keys: vec![],
                values: vec![],
                next: 0,
            };

            let header_data = Page::Header(header).serialize()?;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&header_data)?;

            let root_data = Page::Leaf(root_leaf).serialize()?;
            file.seek(SeekFrom::Start(PAGE_SIZE as u64))?;
            file.write_all(&root_data)?;
            file.flush()?;

            return Ok(Self {
                file,
                cache: HashMap::new(),
                order: Vec::new(),
                dirty: HashSet::new(),
                num_pages: 2,
                free_list: 0,
            });
        }

        let mut pager = Self {
            file,
            cache: HashMap::new(),
            order: Vec::new(),
            dirty: HashSet::new(),
            num_pages: 0,
            free_list: 0,
        };
        let header_data = pager.read_raw_page(0)?;
        let header = pager.parse_header(&header_data)?;
        pager.num_pages = header.num_pages;
        pager.free_list = header.free_list;

        Ok(pager)
    }

    /// Get a mutable reference to the page at `id`, loading it from disk if necessary.
    ///
    /// # Arguments
    ///
    /// * `id` — the page ID to retrieve.
    ///
    /// # Returns
    ///
    /// * `Result<&mut Page>` — the page at the given ID.
    pub(super) fn get_page(&mut self, id: u32) -> Result<&mut Page> {
        // move page to end of order for LRU eviction
        if let Some(pos) = self.order.iter().position(|&x| x == id) {
            self.order.remove(pos);
            self.order.push(id);

        // add new page to cache if not present
        } else {
            if self.cache.len() >= MAX_CACHE {
                self.evict_one()?;
            }

            let page_data = self.read_raw_page(id)?;
            let page = Page::deserialize(&page_data)?;
            self.cache.insert(id, page);
            self.order.push(id);
        }

        Ok(self.cache.get_mut(&id).unwrap())
    }

    /// Allocates a new page and returns its ID.
    ///
    /// # Arguments
    ///
    /// * `page` - The page to be written to the new page ID.
    ///
    /// # Returns
    ///
    /// The ID of the newly allocated page.
    pub(super) fn new_page(&mut self, page: Page) -> Result<u32> {
        if self.cache.len() >= MAX_CACHE {
            self.evict_one()?;
        }

        if self.free_list != 0 {
            let id = self.free_list;
            let next_data = self.read_raw_page(id)?;
            self.free_list = u32::from_le_bytes(next_data[0..4].try_into().unwrap());

            self.write_page(id, &page)?;

            self.cache.insert(id, page);
            self.order.push(id);
            self.dirty.insert(id);

            self.mark_header_dirty();

            Ok(id)
        } else {
            let id = self.num_pages;
            self.num_pages += 1;

            self.write_page(id, &page)?;

            self.cache.insert(id, page);
            self.order.push(id);
            self.dirty.insert(id);

            self.mark_header_dirty();

            Ok(id)
        }
    }

    /// Free a page, adding it to the free list.
    ///
    /// # Arguments
    ///
    /// * `id` — the page ID to free.
    ///
    /// # Returns
    ///
    /// * `Result<()>` — success or error.
    pub(super) fn free_page(&mut self, id: u32) -> Result<()> {
        self.cache.remove(&id);
        self.dirty.remove(&id);
        if let Some(pos) = self.order.iter().position(|&x| x == id) {
            self.order.remove(pos);
        }

        let mut data = [0u8; PAGE_SIZE];
        data[0..4].copy_from_slice(&self.free_list.to_le_bytes());
        self.file
            .seek(SeekFrom::Start(id as u64 * PAGE_SIZE as u64))?;
        self.file.write_all(&data)?;

        self.free_list = id;
        self.mark_header_dirty();

        Ok(())
    }

    /// Mark a page as dirty so it will be flushed to disk.
    ///
    /// # Arguments
    ///
    /// * `id` — the page ID to mark.
    pub(super) fn mark_dirty(&mut self, id: u32) {
        self.dirty.insert(id);
    }

    /// Flush all dirty pages and header to disk.
    ///
    /// # Returns
    ///
    /// * `Result<()>` — success or error.
    fn flush(&mut self) -> Result<()> {
        for &id in &self.dirty.clone() {
            if let Some(page) = self.cache.get(&id) {
                let data = page.serialize()?;
                self.file
                    .seek(SeekFrom::Start(id as u64 * PAGE_SIZE as u64))?;
                self.file.write_all(&data)?;
            }
        }
        self.dirty.clear();

        // Maybe this write_header is no-used, because page 0 also can be marked dirty.
        self.write_header()?;
        self.file.flush()?;

        Ok(())
    }

    /// Evict the least recently used page from the cache.
    ///
    /// If the page is dirty, it is flushed to disk before removal.
    ///
    /// # Returns
    ///
    /// * `Result<()>` — success or error.
    fn evict_one(&mut self) -> Result<()> {
        let id = self.order.remove(0);
        if self.dirty.contains(&id) {
            if let Some(page) = self.cache.get(&id) {
                let data = page.serialize()?;
                self.file
                    .seek(SeekFrom::Start(id as u64 * PAGE_SIZE as u64))?;
                self.file.write_all(&data)?;
            }
            self.dirty.remove(&id);
        }
        self.cache.remove(&id);

        Ok(())
    }

    /// Read a raw 4096-byte page from disk.
    ///
    /// # Arguments
    ///
    /// * `id` — the page ID to read.
    ///
    /// # Returns
    ///
    /// * `Result<[u8; PAGE_SIZE]>` — the raw page data.
    fn read_raw_page(&mut self, id: u32) -> Result<[u8; PAGE_SIZE]> {
        let mut data = [0u8; PAGE_SIZE];
        self.file
            .seek(SeekFrom::Start(id as u64 * PAGE_SIZE as u64))?;
        self.file.read_exact(&mut data)?;

        Ok(data)
    }

    /// Write a serialized page to disk at the given page ID.
    ///
    /// # Arguments
    ///
    /// * `id` — the page ID to write to.
    /// * `page` — the page to serialize and write.
    ///
    /// # Returns
    ///
    /// * `Result<()>` — success or error.
    fn write_page(&mut self, id: u32, page: &Page) -> Result<()> {
        let data = page.serialize()?;
        self.file
            .seek(SeekFrom::Start(id as u64 * PAGE_SIZE as u64))?;
        self.file.write_all(&data)?;

        Ok(())
    }

    /// Parse the raw page 0 bytes into a `HeaderPage`.
    ///
    /// # Arguments
    ///
    /// * `data` — the raw page 0 data.
    ///
    /// # Returns
    ///
    /// * `Result<HeaderPage>` — the parsed header.
    fn parse_header(&self, data: &[u8; PAGE_SIZE]) -> Result<HeaderPage> {
        let page = Page::deserialize(data)?;
        match page {
            Page::Header(h) => Ok(h),
            _ => anyhow::bail!("page 0 is not a header page"),
        }
    }

    /// Update the in-memory header cache and mark page 0 dirty.
    fn mark_header_dirty(&mut self) {
        self.dirty.insert(0);
        let header = HeaderPage {
            magic: MAGIC,
            version: 1,
            num_pages: self.num_pages,
            free_list: self.free_list,
        };
        self.cache.insert(0, Page::Header(header));
    }

    /// Write the current header fields to disk (page 0).
    ///
    /// # Returns
    ///
    /// * `Result<()>` — success or error.
    fn write_header(&mut self) -> Result<()> {
        let header = HeaderPage {
            magic: MAGIC,
            version: 1,
            num_pages: self.num_pages,
            free_list: self.free_list,
        };
        self.write_page(0, &Page::Header(header))?;

        Ok(())
    }
}

impl Drop for Pager {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cleanup(path: &str) {
        let _ = std::fs::remove_file(path);
    }

    fn test_path(name: &str) -> String {
        format!("data/test_pager_{}.db", name)
    }

    #[test]
    fn test_open_new_file() {
        let path = test_path("open_new");
        cleanup(&path);
        let pager = Pager::open(&path).unwrap();
        assert_eq!(pager.num_pages, 2);
        assert_eq!(pager.free_list, 0);
        cleanup(&path);
    }

    #[test]
    fn test_get_and_new_page() {
        let path = test_path("get_new");
        cleanup(&path);
        let mut pager = Pager::open(&path).unwrap();

        let leaf = LeafPage {
            keys: vec![b"k1".to_vec()],
            values: vec![b"v1".to_vec()],
            next: 0,
        };
        let id = pager.new_page(Page::Leaf(leaf)).unwrap();
        assert_eq!(id, 2);

        let page = pager.get_page(id).unwrap();
        match page {
            Page::Leaf(p) => {
                assert_eq!(p.keys, vec![b"k1".to_vec()]);
                assert_eq!(p.values, vec![b"v1".to_vec()]);
            }
            _ => panic!("expected leaf"),
        }
        cleanup(&path);
    }

    #[test]
    fn test_free_and_reuse_page() {
        let path = test_path("free_reuse");
        cleanup(&path);
        let mut pager = Pager::open(&path).unwrap();

        let leaf = LeafPage {
            keys: vec![b"k1".to_vec()],
            values: vec![b"v1".to_vec()],
            next: 0,
        };
        let id = pager.new_page(Page::Leaf(leaf)).unwrap();
        pager.free_page(id).unwrap();
        assert_eq!(pager.free_list, id);

        let leaf2 = LeafPage {
            keys: vec![b"k2".to_vec()],
            values: vec![b"v2".to_vec()],
            next: 0,
        };
        let reused_id = pager.new_page(Page::Leaf(leaf2)).unwrap();
        assert_eq!(reused_id, id);
        cleanup(&path);
    }

    #[test]
    fn test_cache_eviction() {
        let path = test_path("cache_evict");
        cleanup(&path);
        let mut pager = Pager::open(&path).unwrap();

        for i in 0..MAX_CACHE + 10 {
            let leaf = LeafPage {
                keys: vec![b"k".to_vec()],
                values: vec![vec![i as u8]],
                next: 0,
            };
            pager.new_page(Page::Leaf(leaf)).unwrap();
        }

        assert!(pager.cache.len() <= MAX_CACHE);
        cleanup(&path);
    }

    #[test]
    fn test_flush_persistence() {
        let path = test_path("flush");
        cleanup(&path);

        {
            let mut pager = Pager::open(&path).unwrap();
            let leaf = LeafPage {
                keys: vec![b"k".to_vec()],
                values: vec![b"v".to_vec()],
                next: 0,
            };
            pager.new_page(Page::Leaf(leaf)).unwrap();
            // pager.flush().unwrap();
        }

        {
            let pager = Pager::open(&path).unwrap();
            assert_eq!(pager.num_pages, 3);
        }

        cleanup(&path);
    }
}
