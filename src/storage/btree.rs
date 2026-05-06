//! A B+ tree implementation for key-value storage.

use std::{cell::RefCell, fs, path::Path, rc::Rc};

use anyhow::Result;

use super::{
    Bytes, MAX_KEYS_INTERNAL, MAX_KEYS_LEAF, MIN_KEYS_INTERNAL, MIN_KEYS_LEAF,
    page::{InternalPage, LeafPage, Page},
    pager::Pager,
};

/// A B+ tree backed by a shared pager.
///
/// Supports get, put, delete, scan, range scan, and catalog entry management.
///
/// Internal pages hold up to `MAX_KEYS_INTERNAL` keys; leaf pages up to `MAX_KEYS_LEAF`.
pub(crate) struct BPlusTree {
    /// Shared pager for disk I/O.
    pager: Rc<RefCell<Pager>>,
    /// Root page ID of this tree.
    root_page: u32,
}

impl BPlusTree {
    /// Open a B+ tree backed by the database file at `path`.
    ///
    /// # Arguments
    ///
    /// * `path` — path to the database file.
    ///
    /// # Returns
    ///
    /// * `Result<Self>` — a new or opened B+ tree with root page 1.
    pub(crate) fn open(path: &str) -> Result<Self> {
        if let Some(parent) = Path::new(path).parent() {
            fs::create_dir_all(parent)?;
        }

        Ok(Self {
            pager: Rc::new(RefCell::new(Pager::open(path)?)),
            root_page: 1,
        })
    }

    /// Returns the current root page ID.
    ///
    /// # Returns
    ///
    /// * `u32` — the page ID of the root page.
    pub(crate) fn root_page(&self) -> u32 {
        self.root_page
    }

    /// Look up a key and return its value.
    ///
    /// # Arguments
    ///
    /// * `key` — the key to look up.
    ///
    /// # Returns
    ///
    /// * `Result<Option<Bytes>>` — the value if found, or `None`.
    pub(crate) fn get(&self, key: &[u8]) -> Result<Option<Bytes>> {
        let mut current_id = self.root_page;
        loop {
            let next = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(current_id)?;
                match page {
                    Page::Internal(p) => {
                        let i = p
                            .keys
                            .binary_search_by(|k| k.as_slice().cmp(key))
                            .map_or_else(|i| i, |i| i + 1);

                        p.children[i]
                    }
                    Page::Leaf(p) => {
                        return match p.keys.binary_search_by(|k| k.as_slice().cmp(key)) {
                            Ok(pos) => Ok(Some(p.values[pos].clone())),
                            Err(_) => Ok(None),
                        };
                    }
                    _ => anyhow::bail!("unexpected page type"),
                }
            };
            current_id = next;
        }
    }

    /// Insert or update a key-value pair.
    ///
    /// If the key already exists, its value is overwritten. Splits are propagated
    /// up the tree, potentially creating a new root.
    ///
    /// # Arguments
    ///
    /// * `key` — the key to insert or update.
    /// * `value` — the value to associate with the key.
    ///
    /// # Returns
    ///
    /// * `Result<()>` — `Ok(())` if the operation succeeded, or an error.
    pub(crate) fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        let key_vec = key.to_vec();
        // Stack of (page_id, child_index) for the path from root to leaf.
        //
        // page_id is the id of the page on the path.
        //
        // child_index is the index of the child pointer that was followed at that page.
        let mut path: Vec<(u32, usize)> = Vec::new();
        let mut current_id = self.root_page;

        // Phase 1: traverse to leaf
        loop {
            let next = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(current_id)?;
                match page {
                    Page::Internal(p) => {
                        let i = p
                            .keys
                            .binary_search_by(|k| k.as_slice().cmp(&key_vec))
                            .map_or_else(|i| i, |i| i + 1);
                        let child = p.children[i];
                        path.push((current_id, i));
                        Some(child)
                    }
                    Page::Leaf(_) => None,
                    _ => anyhow::bail!("unexpected page type"),
                }
            };
            match next {
                Some(id) => current_id = id,
                None => break,
            }
        }
        let leaf_id = current_id;

        // Phase 2: insert into leaf
        let split_info = {
            let mut p = self.pager.borrow_mut();
            let page = p.get_page(leaf_id)?;
            match page {
                Page::Leaf(p) => match p.keys.binary_search_by(|k| k.as_slice().cmp(&key_vec)) {
                    Ok(pos) => {
                        p.values[pos] = value.to_vec();
                        None
                    }
                    Err(pos) => {
                        p.keys.insert(pos, key_vec.clone());
                        p.values.insert(pos, value.to_vec());
                        if p.keys.len() <= MAX_KEYS_LEAF {
                            None
                        } else {
                            let mid = p.keys.len() / 2;
                            let push_key = p.keys[mid].clone();
                            let new_keys = p.keys.split_off(mid);
                            let new_values = p.values.split_off(mid);
                            let old_next = p.next;
                            p.next = 0;
                            Some((push_key, new_keys, new_values, old_next))
                        }
                    }
                },
                _ => anyhow::bail!("expected leaf page"),
            }
        };
        // mark dirty after scope
        self.pager.borrow_mut().mark_dirty(leaf_id);

        let mut push_key;
        let mut new_page_id;

        if let Some((key, new_keys, new_values, old_next)) = split_info {
            push_key = Some(key);
            new_page_id = self.pager.borrow_mut().new_page(Page::Leaf(LeafPage {
                keys: new_keys,
                values: new_values,
                next: old_next,
            }))?;

            {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(leaf_id)?;
                if let Page::Leaf(page) = page {
                    page.next = new_page_id;
                }
            }
            self.pager.borrow_mut().mark_dirty(leaf_id);
        } else {
            return Ok(());
        }

        // Phase 3: propagate split up
        while let Some(key) = push_key {
            match path.pop() {
                // reached root, need to create new root
                None => {
                    let old_root = self.root_page;
                    let root_id = self.pager.borrow_mut().new_page(Page::Internal(
                        super::page::InternalPage {
                            keys: vec![key],
                            children: vec![old_root, new_page_id],
                        },
                    ))?;
                    self.root_page = root_id;
                    return Ok(());
                }

                // not reached root, insert into parent
                Some((parent_id, child_idx)) => {
                    let split_result = {
                        let mut p = self.pager.borrow_mut();
                        let page = p.get_page(parent_id)?;
                        match page {
                            Page::Internal(p) => {
                                p.keys.insert(child_idx, key.clone());
                                p.children.insert(child_idx + 1, new_page_id);
                                if p.keys.len() <= MAX_KEYS_INTERNAL {
                                    None
                                } else {
                                    let mid = p.keys.len() / 2;
                                    let up_key = p.keys[mid].clone();
                                    let right_keys = p.keys.split_off(mid + 1);
                                    p.keys.pop();
                                    let right_children = p.children.split_off(mid + 1);
                                    Some((up_key, right_keys, right_children))
                                }
                            }
                            _ => anyhow::bail!("expected internal page"),
                        }
                    };
                    self.pager.borrow_mut().mark_dirty(parent_id);

                    match split_result {
                        None => push_key = None,
                        Some((up_key, right_keys, right_children)) => {
                            let right_id =
                                self.pager
                                    .borrow_mut()
                                    .new_page(Page::Internal(InternalPage {
                                        keys: right_keys,
                                        children: right_children,
                                    }))?;
                            push_key = Some(up_key);
                            new_page_id = right_id;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Delete a key from the tree.
    ///
    /// Handles underflow via borrow or merge, and shrinks the root if necessary.
    ///
    /// # Arguments
    ///
    /// * `key` — the key to delete.
    ///
    /// # Returns
    ///
    /// * `Result<()>` — `Ok(())` if the operation succeeded, or an error.
    pub(crate) fn delete(&mut self, key: &[u8]) -> Result<()> {
        let key_vec = key.to_vec();
        let mut path: Vec<(u32, usize)> = Vec::new();
        let mut current_id = self.root_page;

        // Phase 1: traverse to leaf
        loop {
            let next = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(current_id)?;
                match page {
                    Page::Internal(p) => {
                        let i = p
                            .keys
                            .binary_search_by(|k| k.as_slice().cmp(&key_vec))
                            .map_or_else(|i| i, |i| i + 1);
                        let child = p.children[i];
                        path.push((current_id, i));
                        child
                    }
                    Page::Leaf(_) => break,
                    _ => anyhow::bail!("unexpected page type"),
                }
            };
            current_id = next;
        }
        let leaf_id = current_id;

        // Phase 2: delete from leaf
        let (deleted, underflow) = {
            let mut p = self.pager.borrow_mut();
            let page = p.get_page(leaf_id)?;
            match page {
                Page::Leaf(p) => match p.keys.binary_search_by(|k| k.as_slice().cmp(&key_vec)) {
                    Ok(pos) => {
                        p.keys.remove(pos);
                        p.values.remove(pos);
                        (true, p.keys.len() < MIN_KEYS_LEAF)
                    }
                    Err(_) => (false, false),
                },
                _ => (false, false),
            }
        };

        if deleted {
            self.pager.borrow_mut().mark_dirty(leaf_id);
        }

        if !deleted {
            return Ok(());
        }

        if !underflow || path.is_empty() {
            return Ok(());
        }

        // Phase 3: handle underflow cascade
        let mut current_id = leaf_id;
        let mut is_leaf = true;

        while let Some((parent_id, child_idx)) = path.pop() {
            if self.try_borrow_right(parent_id, child_idx, current_id, is_leaf)? {
                return Ok(());
            }
            if self.try_borrow_left(parent_id, child_idx, current_id, is_leaf)? {
                return Ok(());
            }

            let merged_right = self.try_merge_right(parent_id, child_idx, current_id, is_leaf)?;

            let remaining_id = if !merged_right {
                self.do_merge_left(parent_id, child_idx, current_id, is_leaf)?
            } else {
                current_id
            };

            let parent_underflow = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(parent_id)?;
                match page {
                    Page::Internal(p) => p.keys.len() < MIN_KEYS_INTERNAL,
                    _ => false,
                }
            };

            if parent_id == self.root_page {
                self.check_root_shrink()?;
                return Ok(());
            }

            if !parent_underflow {
                return Ok(());
            }

            current_id = remaining_id;
            is_leaf = false;
        }

        Ok(())
    }

    /// Scan the entire tree in key order.
    ///
    /// # Returns
    ///
    /// * `Result<Vec<(Bytes, Bytes)>>` — all key-value pairs in sorted order.
    pub(crate) fn scan(&self) -> Result<Vec<(Bytes, Bytes)>> {
        let mut result = Vec::new();
        let mut leaf_id = self.root_page;

        // search for the leftmost leaf
        loop {
            let next = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(leaf_id)?;
                match page {
                    Page::Internal(p) => p.children[0],
                    Page::Leaf(_) => break,
                    _ => anyhow::bail!("unexpected page type"),
                }
            };
            leaf_id = next;
        }

        while leaf_id != 0 {
            let next = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(leaf_id)?;
                match page {
                    Page::Leaf(p) => {
                        for i in 0..p.keys.len() {
                            result.push((p.keys[i].clone(), p.values[i].clone()));
                        }
                        p.next
                    }
                    _ => anyhow::bail!("expected leaf page"),
                }
            };
            leaf_id = next;
        }

        Ok(result)
    }

    /// Scan keys in the range `[start, end)`.
    ///
    /// Finds the first leaf page with key ≥ `start`, follows the next chain, and collects
    /// all entries until key ≥ `end` (if provided).
    ///
    /// It is only used in index tree.
    ///
    /// # Arguments
    ///
    /// * `start` — inclusive lower bound.
    /// * `end` — exclusive upper bound, or `None` for no upper bound.
    ///
    /// # Returns
    ///
    /// * `Result<Vec<(Bytes, Bytes)>>` — matching key-value pairs in sorted order.
    pub(crate) fn range_scan(
        &self,
        start: &[u8],
        end: Option<&[u8]>,
    ) -> Result<Vec<(Bytes, Bytes)>> {
        let mut result = Vec::new();
        let mut current_id = self.root_page;

        loop {
            let next = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(current_id)?;
                match page {
                    Page::Internal(p) => {
                        let i = p.keys.partition_point(|k| k.as_slice() < start);
                        p.children[i]
                    }
                    Page::Leaf(_) => break,
                    _ => anyhow::bail!("unexpected page type"),
                }
            };
            current_id = next;
        }

        while current_id != 0 {
            let next = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(current_id)?;
                match page {
                    Page::Leaf(p) => {
                        for i in 0..p.keys.len() {
                            if let Some(end) = end
                                && p.keys[i].as_slice() >= end
                            {
                                return Ok(result);
                            }
                            result.push((p.keys[i].clone(), p.values[i].clone()));
                        }
                        p.next
                    }
                    _ => anyhow::bail!("expected leaf page"),
                }
            };
            current_id = next;
        }

        Ok(result)
    }

    /// Shrink the root if it has 0 or 1 children.
    ///
    /// If the root is an internal page with 1 child, that child becomes the new root.
    /// If empty, a fresh empty leaf page is created as the new root.
    ///
    /// # Returns
    ///
    /// * `Result<()>` — `Ok(())` if the operation succeeded, or an error.
    fn check_root_shrink(&mut self) -> Result<()> {
        let root_id = self.root_page;
        let should_shrink = {
            let mut p = self.pager.borrow_mut();
            let page = p.get_page(root_id)?;
            match page {
                Page::Internal(p) => p.children.len() <= 1,
                _ => false,
            }
        };

        if should_shrink {
            let new_root = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(root_id)?;
                match page {
                    Page::Internal(p) => p.children.first().copied(),
                    _ => return Ok(()),
                }
            };

            // len == 1
            if let Some(child) = new_root {
                self.root_page = child;

            // len == 0
            } else {
                let empty_id = self.pager.borrow_mut().new_page(Page::Leaf(LeafPage {
                    keys: vec![],
                    values: vec![],
                    next: 0,
                }))?;
                self.root_page = empty_id;
            }
            self.pager.borrow_mut().free_page(root_id)?;
        }

        Ok(())
    }

    /// Recursively free all pages owned by this tree, starting from `self.root_page`.
    ///
    /// # Returns
    ///
    /// * `Result<()>` — `Ok(())` if the operation succeeded, or an error.
    fn drop_tree(&mut self) -> Result<()> {
        self.drop_page(self.root_page)
    }

    /// Free a page and all its descendants.
    ///
    /// # Arguments
    ///
    /// * `id` — the page ID to free.
    ///
    /// # Returns
    ///
    /// * `Result<()>` — `Ok(())` if the operation succeeded, or an error.
    fn drop_page(&mut self, id: u32) -> Result<()> {
        let children = {
            let mut p = self.pager.borrow_mut();
            let page = p.get_page(id)?;
            match page {
                Page::Internal(page) => page.children.clone(),
                _ => vec![],
            }
        };

        for child in children {
            self.drop_page(child)?;
        }

        self.pager.borrow_mut().free_page(id)?;

        Ok(())
    }

    /// Create a new entry in this catalog tree.
    ///
    /// Allocates a new leaf page as root_page, stores the mapping `key → [root_page(4) + metadata]`.
    ///
    /// # Arguments
    ///
    /// * `key` — catalog key as bytes.
    /// * `metadata` — arbitrary metadata to store alongside the root page ID.
    ///
    /// # Returns
    ///
    /// * `Result<()>` — success or error.
    pub(crate) fn create_entry(&mut self, key: &[u8], metadata: &[u8]) -> Result<()> {
        let root_page = self.pager.borrow_mut().new_page(Page::Leaf(LeafPage {
            keys: vec![],
            values: vec![],
            next: 0,
        }))?;
        let mut value = Vec::with_capacity(4 + metadata.len());
        value.extend_from_slice(&root_page.to_le_bytes());
        value.extend_from_slice(metadata);
        self.put(key, &value)
    }

    /// Look up an entry by key in this catalog tree.
    ///
    /// # Arguments
    ///
    /// * `key` — catalog key as bytes.
    ///
    /// # Returns
    ///
    /// * `Result<Option<(BPlusTree, Bytes)>>` — the tree and metadata, or `None`.
    pub(crate) fn get_entry(&self, key: &[u8]) -> Result<Option<(BPlusTree, Bytes)>> {
        let value = self.get(key)?;
        match value {
            None => Ok(None),
            Some(bytes) => {
                let root_page = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
                let metadata = bytes[4..].to_vec();
                Ok(Some((
                    BPlusTree {
                        pager: Rc::clone(&self.pager),
                        root_page,
                    },
                    metadata,
                )))
            }
        }
    }

    /// Drop an entry from this catalog tree.
    ///
    /// # Arguments
    ///
    /// * `key` — catalog key as bytes.
    ///
    /// # Returns
    ///
    /// * `Result<()>` — `Ok(())` if the operation succeeded, or an error.
    pub(crate) fn drop_entry(&mut self, key: &[u8]) -> Result<()> {
        let value = self.get(key)?;
        match value {
            None => anyhow::bail!("catalog entry not found"),
            Some(bytes) => {
                let root_page = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
                let mut temp = BPlusTree {
                    pager: Rc::clone(&self.pager),
                    root_page,
                };
                temp.drop_tree()?;
                self.delete(key)?;
            }
        }

        Ok(())
    }

    /// Try to borrow an entry from the right sibling.
    ///
    /// If the right sibling exists and has more than `MIN` keys, move one
    /// entry from its head to the end of the underflowed node, then update
    /// the parent separator key.
    ///
    /// # Arguments
    ///
    /// * `parent_id` — page ID of the parent node.
    /// * `child_idx` — index of the underflowed node in the parent's children.
    /// * `current_id` — page ID of the underflowed node.
    /// * `is_leaf` — `true` if the underflowed node is a leaf.
    ///
    /// # Returns
    ///
    /// * `Result<bool>` — `true` if borrowing succeeded, `false` otherwise.
    fn try_borrow_right(
        &mut self,
        parent_id: u32,
        child_idx: usize,
        current_id: u32,
        is_leaf: bool,
    ) -> Result<bool> {
        let right_id = {
            let mut p = self.pager.borrow_mut();
            let page = p.get_page(parent_id)?;
            match page {
                Page::Internal(p) => {
                    if child_idx + 1 < p.children.len() {
                        p.children[child_idx + 1]
                    } else {
                        return Ok(false);
                    }
                }
                _ => return Ok(false),
            }
        };

        let can_borrow = {
            let mut p = self.pager.borrow_mut();
            let page = p.get_page(right_id)?;
            match page {
                Page::Leaf(p) => p.keys.len() > MIN_KEYS_LEAF,
                Page::Internal(p) => p.keys.len() > MIN_KEYS_INTERNAL,
                _ => false,
            }
        };

        if !can_borrow {
            return Ok(false);
        }

        if is_leaf {
            let (borrowed_key, borrowed_val) = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(right_id)?;
                match page {
                    Page::Leaf(p) => {
                        let k = p.keys.remove(0);
                        let v = p.values.remove(0);
                        (k, v)
                    }
                    _ => return Ok(false),
                }
            };
            self.pager.borrow_mut().mark_dirty(right_id);

            {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(current_id)?;
                if let Page::Leaf(p) = page {
                    p.keys.push(borrowed_key);
                    p.values.push(borrowed_val);
                }
            }
            self.pager.borrow_mut().mark_dirty(current_id);

            let new_sep = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(right_id)?;
                match page {
                    Page::Leaf(p) => p.keys[0].clone(),
                    _ => return Ok(false),
                }
            };

            {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(parent_id)?;
                if let Page::Internal(p) = page {
                    p.keys[child_idx] = new_sep;
                }
            }
            self.pager.borrow_mut().mark_dirty(parent_id);
        } else {
            let sep_key = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(parent_id)?;
                match page {
                    Page::Internal(p) => p.keys[child_idx].clone(),
                    _ => return Ok(false),
                }
            };

            let (borrowed_key, borrowed_child) = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(right_id)?;
                match page {
                    Page::Internal(p) => {
                        let k = p.keys.remove(0);
                        let c = p.children.remove(0);
                        (k, c)
                    }
                    _ => return Ok(false),
                }
            };
            self.pager.borrow_mut().mark_dirty(right_id);

            {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(current_id)?;
                if let Page::Internal(p) = page {
                    p.keys.push(sep_key);
                    p.children.push(borrowed_child);
                }
            }
            self.pager.borrow_mut().mark_dirty(current_id);

            {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(parent_id)?;
                if let Page::Internal(p) = page {
                    p.keys[child_idx] = borrowed_key;
                }
            }
            self.pager.borrow_mut().mark_dirty(parent_id);
        }

        Ok(true)
    }

    /// Try to borrow an entry from the left sibling.
    ///
    /// If the left sibling exists and has more than `MIN` keys, move one
    /// entry from its tail to the head of the underflowed node, then update
    /// the parent separator key.
    ///
    /// # Arguments
    ///
    /// * `parent_id` — page ID of the parent node.
    /// * `child_idx` — index of the underflowed node in the parent's children.
    /// * `current_id` — page ID of the underflowed node.
    /// * `is_leaf` — `true` if the underflowed node is a leaf.
    ///
    /// # Returns
    ///
    /// * `Result<bool>` — `true` if borrowing succeeded, `false` otherwise.
    fn try_borrow_left(
        &mut self,
        parent_id: u32,
        child_idx: usize,
        current_id: u32,
        is_leaf: bool,
    ) -> Result<bool> {
        if child_idx == 0 {
            return Ok(false);
        }

        let left_id = {
            let mut p = self.pager.borrow_mut();
            let page = p.get_page(parent_id)?;
            match page {
                Page::Internal(p) => p.children[child_idx - 1],
                _ => return Ok(false),
            }
        };

        let can_borrow = {
            let mut p = self.pager.borrow_mut();
            let page = p.get_page(left_id)?;
            match page {
                Page::Leaf(p) => p.keys.len() > MIN_KEYS_LEAF,
                Page::Internal(p) => p.keys.len() > MIN_KEYS_INTERNAL,
                _ => false,
            }
        };

        if !can_borrow {
            return Ok(false);
        }

        if is_leaf {
            let (borrowed_key, borrowed_val) = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(left_id)?;
                match page {
                    Page::Leaf(p) => {
                        let k = p.keys.pop().unwrap();
                        let v = p.values.pop().unwrap();
                        (k, v)
                    }
                    _ => return Ok(false),
                }
            };
            self.pager.borrow_mut().mark_dirty(left_id);

            {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(current_id)?;
                if let Page::Leaf(p) = page {
                    p.keys.insert(0, borrowed_key);
                    p.values.insert(0, borrowed_val);
                }
            }
            self.pager.borrow_mut().mark_dirty(current_id);

            let new_sep = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(current_id)?;
                match page {
                    Page::Leaf(p) => p.keys[0].clone(),
                    _ => return Ok(false),
                }
            };

            {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(parent_id)?;
                if let Page::Internal(p) = page {
                    p.keys[child_idx - 1] = new_sep;
                }
            }
            self.pager.borrow_mut().mark_dirty(parent_id);
        } else {
            let sep_key = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(parent_id)?;
                match page {
                    Page::Internal(p) => p.keys[child_idx - 1].clone(),
                    _ => return Ok(false),
                }
            };

            let (borrowed_key, borrowed_child) = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(left_id)?;
                match page {
                    Page::Internal(p) => {
                        let k = p.keys.pop().unwrap();
                        let c = p.children.pop().unwrap();
                        (k, c)
                    }
                    _ => return Ok(false),
                }
            };
            self.pager.borrow_mut().mark_dirty(left_id);

            {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(current_id)?;
                if let Page::Internal(p) = page {
                    p.keys.insert(0, sep_key);
                    p.children.insert(0, borrowed_child);
                }
            }
            self.pager.borrow_mut().mark_dirty(current_id);

            {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(parent_id)?;
                if let Page::Internal(p) = page {
                    p.keys[child_idx - 1] = borrowed_key;
                }
            }
            self.pager.borrow_mut().mark_dirty(parent_id);
        }

        Ok(true)
    }

    /// Merge the underflowed node with its right sibling.
    ///
    /// All entries from the right sibling are moved into the current node.
    /// The right sibling is freed and removed from the parent. For internal
    /// nodes the parent separator key is pulled down between the two halves.
    ///
    /// # Arguments
    ///
    /// * `parent_id` — page ID of the parent node.
    /// * `child_idx` — index of the underflowed node in the parent's children.
    /// * `current_id` — page ID of the underflowed node.
    /// * `is_leaf` — `true` if the underflowed node is a leaf.
    ///
    /// # Returns
    ///
    /// * `Result<bool>` — `false` if there is no right sibling to merge with.
    fn try_merge_right(
        &mut self,
        parent_id: u32,
        child_idx: usize,
        current_id: u32,
        is_leaf: bool,
    ) -> Result<bool> {
        let right_id = {
            let mut p = self.pager.borrow_mut();
            let page = p.get_page(parent_id)?;
            match page {
                Page::Internal(p) => {
                    if child_idx + 1 < p.children.len() {
                        p.children[child_idx + 1]
                    } else {
                        return Ok(false);
                    }
                }
                _ => return Ok(false),
            }
        };

        if is_leaf {
            let (right_keys, right_values, right_next) = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(right_id)?;
                match page {
                    Page::Leaf(p) => {
                        let k = p.keys.clone();
                        let v = p.values.clone();
                        let n = p.next;
                        (k, v, n)
                    }
                    _ => return Ok(false),
                }
            };

            {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(current_id)?;
                if let Page::Leaf(p) = page {
                    p.keys.extend(right_keys);
                    p.values.extend(right_values);
                    p.next = right_next;
                }
            }
            self.pager.borrow_mut().mark_dirty(current_id);
        } else {
            let (right_keys, right_children, sep_key) = {
                let mut p = self.pager.borrow_mut();
                let sep = match p.get_page(parent_id)? {
                    Page::Internal(p) => p.keys[child_idx].clone(),
                    _ => return Ok(false),
                };
                let page = p.get_page(right_id)?;
                match page {
                    Page::Internal(p) => (p.keys.clone(), p.children.clone(), sep),
                    _ => return Ok(false),
                }
            };

            {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(current_id)?;
                if let Page::Internal(p) = page {
                    p.keys.push(sep_key);
                    p.keys.extend(right_keys);
                    p.children.extend(right_children);
                }
            }
            self.pager.borrow_mut().mark_dirty(current_id);
        }

        self.pager.borrow_mut().free_page(right_id)?;

        {
            let mut p = self.pager.borrow_mut();
            let page = p.get_page(parent_id)?;
            if let Page::Internal(p) = page {
                p.keys.remove(child_idx);
                p.children.remove(child_idx + 1);
            }
        }
        self.pager.borrow_mut().mark_dirty(parent_id);

        Ok(true)
    }

    /// Merge the underflowed node into its left sibling.
    ///
    /// All entries from the current node are moved into the left sibling.
    /// The current node is freed and removed from the parent. For internal
    /// nodes the parent separator key is pulled down between the two halves.
    ///
    /// # Arguments
    ///
    /// * `parent_id` — page ID of the parent node.
    /// * `child_idx` — index of the underflowed node in the parent's children.
    /// * `current_id` — page ID of the underflowed node.
    /// * `is_leaf` — `true` if the underflowed node is a leaf.
    ///
    /// # Returns
    ///
    /// * `Result<u32>` — page ID of the surviving (left sibling) node.
    fn do_merge_left(
        &mut self,
        parent_id: u32,
        child_idx: usize,
        current_id: u32,
        is_leaf: bool,
    ) -> Result<u32> {
        if child_idx == 0 {
            return Ok(current_id);
        }

        let left_id = {
            let mut p = self.pager.borrow_mut();
            let page = p.get_page(parent_id)?;
            match page {
                Page::Internal(p) => p.children[child_idx - 1],
                _ => anyhow::bail!("no left sibling"),
            }
        };

        if is_leaf {
            let (current_keys, current_values, current_next) = {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(current_id)?;
                match page {
                    Page::Leaf(p) => {
                        let k = p.keys.clone();
                        let v = p.values.clone();
                        let n = p.next;
                        (k, v, n)
                    }
                    _ => return Ok(current_id),
                }
            };

            {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(left_id)?;
                if let Page::Leaf(p) = page {
                    p.keys.extend(current_keys);
                    p.values.extend(current_values);
                    p.next = current_next;
                }
            }
            self.pager.borrow_mut().mark_dirty(left_id);
        } else {
            let (current_keys, current_children, sep_key) = {
                let mut p = self.pager.borrow_mut();
                let sep = match p.get_page(parent_id)? {
                    Page::Internal(p) => p.keys[child_idx - 1].clone(),
                    _ => return Ok(current_id),
                };
                let page = p.get_page(current_id)?;
                match page {
                    Page::Internal(p) => (p.keys.clone(), p.children.clone(), sep),
                    _ => return Ok(current_id),
                }
            };

            {
                let mut p = self.pager.borrow_mut();
                let page = p.get_page(left_id)?;
                if let Page::Internal(p) = page {
                    p.keys.push(sep_key);
                    p.keys.extend(current_keys);
                    p.children.extend(current_children);
                }
            }
            self.pager.borrow_mut().mark_dirty(left_id);
        }

        self.pager.borrow_mut().free_page(current_id)?;

        {
            let mut p = self.pager.borrow_mut();
            let page = p.get_page(parent_id)?;
            if let Page::Internal(p) = page {
                p.keys.remove(child_idx - 1);
                p.children.remove(child_idx);
            }
        }
        self.pager.borrow_mut().mark_dirty(parent_id);

        Ok(left_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cleanup(path: &str) {
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_put_and_get() {
        let path = "data/test_btree_putget.db";
        cleanup(path);
        {
            let mut tree = BPlusTree::open(path).unwrap();
            tree.put(b"hello", b"world").unwrap();
            tree.put(b"foo", b"bar").unwrap();
        }
        {
            let tree = BPlusTree::open(path).unwrap();
            assert_eq!(tree.get(b"hello").unwrap(), Some(b"world".to_vec()));
            assert_eq!(tree.get(b"foo").unwrap(), Some(b"bar".to_vec()));
            assert_eq!(tree.get(b"nope").unwrap(), None);
        }
        cleanup(path);
    }

    #[test]
    fn test_update() {
        let path = "data/test_btree_update.db";
        cleanup(path);
        let mut tree = BPlusTree::open(path).unwrap();
        tree.put(b"key", b"val1").unwrap();
        tree.put(b"key", b"val2").unwrap();
        assert_eq!(tree.get(b"key").unwrap(), Some(b"val2".to_vec()));
        cleanup(path);
    }

    #[test]
    fn test_delete() {
        let path = "data/test_btree_delete.db";
        cleanup(path);
        let mut tree = BPlusTree::open(path).unwrap();
        tree.put(b"k1", b"v1").unwrap();
        tree.put(b"k2", b"v2").unwrap();
        tree.put(b"k3", b"v3").unwrap();
        tree.delete(b"k2").unwrap();
        assert_eq!(tree.get(b"k1").unwrap(), Some(b"v1".to_vec()));
        assert_eq!(tree.get(b"k2").unwrap(), None);
        assert_eq!(tree.get(b"k3").unwrap(), Some(b"v3".to_vec()));
        cleanup(path);
    }

    #[test]
    fn test_scan() {
        let path = "data/test_btree_scan.db";
        cleanup(path);
        let mut tree = BPlusTree::open(path).unwrap();
        tree.put(b"c", b"3").unwrap();
        tree.put(b"a", b"1").unwrap();
        tree.put(b"b", b"2").unwrap();
        let result = tree.scan().unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].0, b"a".to_vec());
        assert_eq!(result[1].0, b"b".to_vec());
        assert_eq!(result[2].0, b"c".to_vec());
        cleanup(path);
    }

    #[test]
    fn test_large_insert() {
        let path = "data/test_btree_large.db";
        cleanup(path);
        let mut tree = BPlusTree::open(path).unwrap();
        for i in 0..500u32 {
            let key = i.to_be_bytes().to_vec();
            let val = (i * 2).to_be_bytes().to_vec();
            tree.put(&key, &val).unwrap();
        }
        for i in 0..500u32 {
            let key = i.to_be_bytes().to_vec();
            let val = tree.get(&key).unwrap();
            assert_eq!(val, Some((i * 2).to_be_bytes().to_vec()));
        }
        cleanup(path);
    }

    #[test]
    fn test_catalog_create_and_get_entry() {
        let path = "data/test_btree_catalog1.db";
        cleanup(path);
        let mut cat = BPlusTree::open(path).unwrap();
        cat.create_entry(b"users", &1u64.to_le_bytes()).unwrap();

        let (mut users, _) = cat.get_entry(b"users").unwrap().unwrap();
        users.put(b"k1", b"v1").unwrap();
        users.put(b"k2", b"v2").unwrap();
        assert_eq!(users.get(b"k1").unwrap(), Some(b"v1".to_vec()));
        assert_eq!(users.get(b"k2").unwrap(), Some(b"v2".to_vec()));

        cleanup(path);
    }

    #[test]
    fn test_catalog_drop_entry() {
        let path = "data/test_btree_catalog2.db";
        cleanup(path);

        let mut cat = BPlusTree::open(path).unwrap();
        cat.create_entry(b"users", &1u64.to_le_bytes()).unwrap();
        assert!(cat.get_entry(b"users").unwrap().is_some());
        cat.drop_entry(b"users").unwrap();
        assert!(cat.get_entry(b"users").unwrap().is_none());

        cleanup(path);
    }

    #[test]
    fn test_catalog_multi_table() {
        let path = "data/test_btree_catalog3.db";
        cleanup(path);
        let mut cat = BPlusTree::open(path).unwrap();
        cat.create_entry(b"t1", &1u64.to_le_bytes()).unwrap();
        cat.create_entry(b"t2", &1u64.to_le_bytes()).unwrap();

        let (mut t1, _) = cat.get_entry(b"t1").unwrap().unwrap();
        t1.put(b"a", b"1").unwrap();
        let (mut t2, _) = cat.get_entry(b"t2").unwrap().unwrap();
        t2.put(b"b", b"2").unwrap();

        assert_eq!(t1.get(b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(t2.get(b"b").unwrap(), Some(b"2".to_vec()));
        assert_eq!(t1.get(b"b").unwrap(), None);

        cleanup(path);
    }
}
