use core::str::from_utf8;
use std::time::{SystemTime, UNIX_EPOCH};
use lmfu::LiteMap;

use super::internals::{
    Result, Error, Mode, Directory, Path, TreeIter, Hash, CommitField, FileType,
    ObjectStore, EntryType, Write, ObjectType, get_commit_field_hash,
};

/// Local repository residing in memory
pub struct Repository {
    pub(crate) directories: LiteMap<Hash, Directory>,
    pub(crate) objects: ObjectStore,
    pub(crate) staged: ObjectStore,
    pub(crate) upstream_head: Hash,
    pub(crate) head: Hash,
    pub(crate) root: Option<Hash>,
}

impl Repository {
    /// Creates an empty repository.
    pub fn new() -> Self {
        Self {
            directories: LiteMap::new(),
            objects: ObjectStore::new(),
            staged: ObjectStore::new(),
            upstream_head: Hash::zero(),
            head: Hash::zero(),
            root: None,
        }
    }

    pub (crate) fn any_store_get(&self, hash: Hash, obj_type: ObjectType) -> Option<&[u8]> {
        match self.staged.get_as(hash, obj_type) {
            Some(entries) => Some(entries),
            None => self.objects.get_as(hash, obj_type),
        }
    }

    pub(crate) fn try_find_dir(&self, hash: Hash) -> Result<Option<Directory>> {
        let mut iter = match self.any_store_get(hash, ObjectType::Tree) {
            Some(entries) => TreeIter::new(entries),
            None => return Ok(None),
        };

        let mut dir = Directory::new();

        while let Some((node, hash, mode)) = iter.next()? {
            dir.insert(node.into(), (hash, mode));
        }

        Ok(Some(dir))
    }

    pub(crate) fn find_dir(&self, hash: Hash) -> Result<Directory> {
        let dir = self.try_find_dir(hash)?;
        
        if dir.is_none() {
            log::warn!("Missing directory for hash {}", hash);
        }

        Ok(dir.unwrap_or(Directory::new()))
    }

    pub(crate) fn remove_dir(&mut self, dir_hash: Hash) -> Result<Directory> {
        match self.directories.remove(&dir_hash) {
            Some(dir) => Ok(dir),
            None => self.find_dir(dir_hash),
        }
    }

    pub(crate) fn get_dir(&mut self, hash: Hash) -> Result<Option<&Directory>> {
        if let None = self.directories.get(&hash) {
            if let Some(dir) = self.try_find_dir(hash)? {
                self.directories.insert(hash, dir);
            }
        }

        Ok(self.directories.get(&hash))
    }

    pub(crate) fn get_commit_root(&self, commit_hash: Hash) -> Result<Option<Hash>> {
        match self.objects.get_as(commit_hash, ObjectType::Commit) {
            Some(commit) => match get_commit_field_hash(commit, CommitField::Tree)? {
                Some(hash) => Ok(Some(hash)),
                None => Err(Error::InvalidObject),
            },
            None => Ok(None),
        }
    }

    pub(crate) fn find_in_dir(&mut self, dir: Hash, node: &str, filter: EntryType) -> Result<(Hash, Mode)> {
        match self.get_dir(dir)? {
            Some(directory) => match directory.get(node) {
                Some((hash, mode)) => match mode.matches(filter) {
                    true => Ok((*hash, *mode)),
                    false => {
                        log::error!("wrong file type for {}: {:?} doesn't match {:?}", node, mode, filter);
                        Err(Error::PathError)
                    },
                },
                None => Err(Error::PathError),
            },
            None => Err(Error::MissingObject),
        }
    }

    /// Returns an iterator on the contents of a directory
    /// that was staged or commited before.
    ///
    /// Returns `PathError` if the path leads to nowhere.
    pub fn read_dir(&mut self, path: &str, entry_type: EntryType) -> Result<impl Iterator<Item = (Mode, &str)>> {
        let path = Path::new(path);
        let mut current = self.root.ok_or(Error::PathError)?;

        for subdir in path.all() {
            current = self.find_in_dir(current, subdir, EntryType::Directory)?.0;
        }

        let directory = self.get_dir(current)?.ok_or(Error::MissingObject)?;
        Ok(directory.iter().filter_map(move |(node, (_, mode))| {
            match mode.matches(entry_type) {
                true => Some((*mode, node.as_str())),
                false => None,
            }
        }))
    }

    /// Returns the content of a file that was staged or commited before.
    ///
    /// Returns `PathError` if the path leads to nowhere.
    pub fn read_file(&mut self, path: &str) -> Result<&[u8]> {
        let path = Path::new(path);
        let mut current = self.root.ok_or(Error::PathError)?;

        for subdir in path.dirs()? {
            current = self.find_in_dir(current, subdir, EntryType::Directory)?.0;
        }

        let (hash, _mode) = self.find_in_dir(current, path.file()?, EntryType::File)?;
        self.any_store_get(hash, ObjectType::Blob).ok_or(Error::MissingObject)
    }

    /// Returns the content of a textual file that was staged or commited before.
    ///
    /// Returns `PathError` if the path leads to nowhere.
    /// Returns `InvalidObject` if the file contains non-utf-8 bytes.
    pub fn read_text(&mut self, path: &str) -> Result<&str> {
        match from_utf8(self.read_file(path)?) {
            Ok(string) => Ok(string),
            Err(_) => Err(Error::InvalidObject),
        }
    }

    pub(crate) fn find_committed_hash_root(&self, mut hash: Hash) -> Option<Hash> {
        while let Some(entry) = self.staged.get(hash) {
            hash = entry.delta_hint()?;
        }

        Some(hash)
    }

    pub(crate) fn update_dir<'a, I: Iterator<Item = &'a str>>(
        &mut self,
        mut directory: Directory,
        steps: &mut I,
        file_name: &str,
        data: Option<(Vec<u8>, FileType)>,
    ) -> Result<Option<Directory>> {
        let mut result = None;

        let step = steps.next();

        let node = step.unwrap_or(file_name);
        let prev_hash = directory.get(node).map(|(hash, _mode)| *hash);
        let delta_hint = prev_hash.and_then(|hash| self.find_committed_hash_root(hash));

        if step.is_some() {
            let subdir = match prev_hash {
                // no path error: use the existing dir
                Some(hash) => self.remove_dir(hash)?,
                // path error: create the dir
                None => Directory::new(),
            };

            if let Some(subdir) = self.update_dir(subdir, steps, file_name, data)? {
                let hash = self.staged.serialize_directory(&subdir, delta_hint);
                self.directories.insert(hash, subdir);
                result = Some((hash, Mode::Directory));
            }
        } else {
            if let Some((data, ft)) = data {
                let hash = self.staged.insert(ObjectType::Blob, data.into(), delta_hint);
                result = Some((hash, ft.into()));
            }
        }

        Ok(if let Some((hash, mode)) = result {
            if self.objects.has(hash) {
                self.staged.remove(hash);
            }

            directory.insert(node.into(), (hash, mode));
            Some(directory)
        } else {
            directory.remove(node);
            match directory.is_empty() {
                true => None,
                false => Some(directory),
            }
        })
    }

    /// Place a new file in the workspace, which will be staged
    /// until the next call to [`Self::commit`].
    ///
    /// - Missing directories are created as needed.
    /// - If `data` is `None`, any existing file at this `path`
    /// will be staged as deleted. If this leads to directories
    /// becoming empty, they will be deleted as well.
    ///
    /// Should only fail if the repository was already corrupted.
    pub fn stage(&mut self, path: &str, data: Option<(Vec<u8>, FileType)>) -> Result<()> {
        let path = Path::new(path);

        let root_dir = match self.root {
            Some(hash) => self.remove_dir(hash)?,
            None => Directory::new(),
        };

        let file_name = path.file()?;
        let mut subdirs = path.dirs()?;

        if let Some(root_dir) = self.update_dir(root_dir, &mut subdirs, file_name, data)? {
            let prev_hash = self.root.and_then(|h| self.find_committed_hash_root(h));
            let hash = self.staged.serialize_directory(&root_dir, prev_hash);
            if self.objects.has(hash) {
                self.staged.remove(hash);
            }

            self.directories.insert(hash, root_dir);
            self.root = Some(hash);
        } else {
            self.root = None;
        }

        Ok(())
    }

    pub(crate) fn commit_object(&mut self, hash: Hash) {
        if let Some(dir_entry) = self.staged.remove(hash) {
            if dir_entry.obj_type() == ObjectType::Tree {

                // mem::replace
                // this unwrap is questionable
                let dir = self.directories.insert(hash, Directory::new()).unwrap();

                for (hash, _mode) in dir.iter_values() {
                    self.commit_object(*hash);
                }

                // mem::replace
                self.directories.insert(hash, dir).unwrap();
            }

            self.objects.insert_entry(dir_entry);
        }
    }

    /// Creates a new commit which saves staged files into the
    /// repository.
    ///
    /// - If `timestamp` is `None`, the current time will be used
    /// instead.
    /// - If one of the strings in `author` & `committer` contain
    /// invalid characters (`<`, `>` or `\n`), this returns
    /// `InvalidObject` immediately.
    pub fn commit(
        &mut self,
        message: &str,
        author: (&str, &str),
        committer: (&str, &str),
        timestamp: Option<u64>,
    ) -> Result<Hash> {
        let timestamp = timestamp.unwrap_or_else(|| {
            let now = SystemTime::now();
            match now.duration_since(UNIX_EPOCH) {
                Ok(duration) => duration.as_secs(),
                _ => 0,
            }
        });

        for string in [author.0, author.1, committer.0, committer.1] {
            let has_newline = string.contains('\n');
            let has_open = string.contains('<');
            let has_close = string.contains('>');
            if has_newline || has_open || has_close {
                return Err(Error::InvalidObject);
            }
        }

        let mut serialized = Vec::new();

        if let Some(root) = self.root {
            if Some(root) != self.get_commit_root(self.head).unwrap() {
                self.commit_object(root);
            }
        }

        let root = self.root.unwrap_or(Hash::zero());
        write!(&mut serialized, "tree {}\n", root).unwrap();

        if !self.head.is_zero() {
            write!(&mut serialized, "parent {}\n", self.head).unwrap();
        }

        write!(&mut serialized, "author {} <{}> {} +0000\n", author.0, author.1, timestamp).unwrap();
        write!(&mut serialized, "committer {} <{}> {} +0000\n", committer.0, committer.1, timestamp).unwrap();
        write!(&mut serialized, "\n{}\n", message).unwrap();

        self.head = self.objects.insert(ObjectType::Commit, serialized.into(), None);

        Ok(self.head)
    }

    /// Resets the current commit to the branch head in upstream
    pub fn discard_commits(&mut self) {
        self.head = self.upstream_head;
    }

    /// Discard changes that weren't commited
    pub fn discard_changes(&mut self) {
        self.staged = ObjectStore::new();
        self.directories.clear();
        self.root = self.get_commit_root(self.head).unwrap();
    }

    /// Resets the clone to the upstream state
    pub fn discard(&mut self) {
        self.discard_commits();
        self.discard_changes();
    }
}