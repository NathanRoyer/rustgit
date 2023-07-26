use core::{fmt, array::from_fn, str::from_utf8};
use lmfu::LiteMap;
use sha1::{Sha1, Digest};

use super::internals::{Result, Error, Directory, Write, Mode};

/// The key to a git object
///
/// Example: `dcf3cb0c8270c187003d84fd359e5bb3904fe42a`.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Hash([u32; 5]);

impl Hash {
    pub fn new(bytes: [u8; 20]) -> Self {
        let mut iter = bytes.chunks(4);
        Self(from_fn(|_i| {
            let mut u32_bytes = [0; 4];
            u32_bytes.copy_from_slice(iter.next().unwrap());
            u32::from_ne_bytes(u32_bytes)
        }))
    }

    pub fn zero() -> Self {
        Self::new([0; 20])
    }

    pub fn is_zero(&self) -> bool {
        *self == Self::zero()
    }

    /// Tries to parse a string into a hash.
    ///
    /// The string must be 40-characters long and only
    /// contain hexadecimal digits.
    pub fn from_hex(mut hex: &str) -> Option<Self> {
        if hex.len() == 40 && hex.is_ascii() {
            let mut array = [0; 5];

            for j in 0..5 {
                let mut u32_bytes = [0; 4];

                for i in 0..4 {
                    let hex_byte = &hex[i * 2..][..2];
                    u32_bytes[i] = u8::from_str_radix(hex_byte, 16).ok()?;
                }

                array[j] = u32::from_ne_bytes(u32_bytes);
                hex = &hex[8..];
            }

            Some(Self(array))
        } else {
            None
        }
    }

    fn first_byte(&self) -> usize {
        self.0[0].to_ne_bytes()[0] as _
    }

    pub fn to_bytes(&self) -> [u8; 20] {
        let mut array = [0; 20];

        let mut i = 0;
        for dword in self.0 {
            for byte in dword.to_ne_bytes() {
                array[i] = byte;
                i += 1;
            }
        }

        array
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.to_bytes() {
            write!(f, "{:02x}", byte)?;
        }

        Ok(())
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ObjectType {
    Commit,
    Tree,
    Blob,
    Tag,
}

impl fmt::Display for ObjectType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", match self {
            ObjectType::Commit => "commit",
            ObjectType::Tree => "tree",
            ObjectType::Blob => "blob",
            ObjectType::Tag => "tag",
        })
    }
}

pub struct Object {
    obj_type: ObjectType,
    content: Box<[u8]>,
    delta_hint: Hash,
}

impl Object {
    pub fn obj_type(&self) -> ObjectType {
        self.obj_type
    }

    pub fn content(&self) -> &[u8] {
        &*self.content
    }

    pub fn delta_hint(&self) -> Option<Hash> {
        match self.delta_hint.is_zero() {
            true => None,
            false => Some(self.delta_hint),
        }
    }
}

pub struct ObjectStore([LiteMap<Hash, Object>; 256]);

impl ObjectStore {
    pub fn new() -> Self {
        Self(from_fn(|_| LiteMap::new()))
    }

    pub fn serialize_directory(&mut self, dir: &Directory, delta_hint: Option<Hash>) -> Hash {
        let mut serialized = Vec::new();

        for (node, (hash, mode)) in dir.iter() {
            let mode = *mode as u32;
            write!(&mut serialized, "{:o} {}\0", mode, node).unwrap();

            for byte in hash.to_bytes() {
                serialized.push(byte);
            }
        }

        self.insert(ObjectType::Tree, serialized.into_boxed_slice(), delta_hint)
    }

    pub fn hash(&self, obj_type: ObjectType, content: &[u8]) -> Hash {
        let mut hasher = Sha1::new();
        write!(&mut hasher, "{} {}\0", obj_type, content.len()).unwrap();
        hasher.update(content);
        Hash::new(hasher.finalize().into())
    }

    pub fn insert_entry(&mut self, entry: Object) -> Hash {
        let hash = self.hash(entry.obj_type, &entry.content);
        self.0[hash.first_byte()].insert(hash, entry);
        hash
    }

    pub fn insert(
        &mut self,
        obj_type: ObjectType,
        content: Box<[u8]>,
        delta_hint: Option<Hash>,
    ) -> Hash {
        let delta_hint = delta_hint.unwrap_or(Hash::zero());
        self.insert_entry(Object {
            obj_type,
            content,
            delta_hint,
        })
    }

    pub fn get(&self, object: Hash) -> Option<&Object> {
        self.0[object.first_byte()].get(&object)
    }

    pub fn has(&self, object: Hash) -> bool {
        self.0[object.first_byte()].contains_key(&object)
    }

    pub fn get_as(&self, object: Hash, obj_type: ObjectType) -> Option<&[u8]> {
        match self.get(object) {
            Some(entry) => match entry.obj_type == obj_type {
                true => Some(&entry.content),
                false => {
                    log::warn!("Object {} was expected to be a {:?} but it's actually a {:?}", object, obj_type, entry.obj_type);
                    None
                },
            },
            None => None,
        }
    }

    pub fn remove(&mut self, object: Hash) -> Option<Object> {
        self.0[object.first_byte()].remove(&object)
    }
}

pub struct TreeIter<'a> {
    entries: &'a [u8],
}

impl<'a> TreeIter<'a> {
    pub fn new(tree_object: &'a [u8]) -> TreeIter<'a> {
        Self {
            entries: tree_object,
        }
    }

    pub fn next(&mut self) -> Result<Option<(&'a str, Hash, Mode)>> {
        let inv_bytes = Error::InvalidObject;

        if self.entries.len() > 0 {
            let i = self.entries.iter().position(|c| *c == b'\0').ok_or(inv_bytes)?;
            let (description, other_bytes) = self.entries.split_at(i);

            let description = from_utf8(description).ok().ok_or(inv_bytes)?;
            let (mode, node) = description.split_once(' ').ok_or(inv_bytes)?;

            let mut hash_bytes = [0; 20];
            hash_bytes.copy_from_slice(other_bytes.get(1..21).ok_or(inv_bytes)?);
            let hash = Hash::new(hash_bytes);

            let mode = match mode {
                "040000" | "40000" => Mode::Directory,
                "100644" => Mode::RegularFile,
                "100664" => Mode::GroupWriteableFile,
                "100755" => Mode::ExecutableFile,
                "120000" => Mode::SymbolicLink,
                "160000" => Mode::Gitlink,
                _ => {
                    log::error!("Invalid mode in directory: {}", mode);
                    return Err(inv_bytes);
                },
            };

            self.entries = other_bytes.get(21..).ok_or(inv_bytes)?;

            Ok(Some((node, hash, mode)))
        } else {
            Ok(None)
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum CommitField {
    Tree,
    Parent(usize),
    Author,
    AuthorEmail,
    AuthorTimezone,
    AuthorTimestamp,
    Committer,
    CommitterEmail,
    CommitterTimestamp,
    CommitterTimezone,
    Message,
}

pub fn get_commit_field<'a>(commit: &'a [u8], field: CommitField) -> Result<Option<&'a str>> {
    let inv_bytes = Error::InvalidObject;
    let text = from_utf8(commit).ok().ok_or(inv_bytes)?;
    let (metadata, message) = text.split_once("\n\n").ok_or(inv_bytes)?;

    if let CommitField::Message = field {
        Ok(match message {
            "" => None,
            msg => Some(msg),
        })
    } else {
        let field_name = match field {
            CommitField::Tree => "tree",
            CommitField::Parent(_) => "parent",
            CommitField::Author |
            CommitField::AuthorEmail |
            CommitField::AuthorTimestamp |
            CommitField::AuthorTimezone => "author",
            CommitField::Committer |
            CommitField::CommitterEmail |
            CommitField::CommitterTimestamp |
            CommitField::CommitterTimezone => "committer",
            CommitField::Message => unreachable!(),
        };

        let mut parent_index = 0;
        for line in metadata.lines() {
            let (key, value) = line.split_once(' ').ok_or(inv_bytes)?;

            if key != field_name {
                continue;
            }

            match field {
                CommitField::Message => unreachable!(),
                CommitField::Tree => return Ok(Some(value)),
                CommitField::Parent(n) => match n == parent_index {
                    true => return Ok(Some(value)),
                    false => parent_index += 1,
                },
                _ => {
                    let (name, value) = value.split_once(" <").ok_or(inv_bytes)?;
                    let (email, value) = value.split_once("> ").ok_or(inv_bytes)?;
                    let (timestamp, timezone) = value.split_once(' ').ok_or(inv_bytes)?;
                    return Ok(Some(match field {
                        CommitField::Author |
                        CommitField::Committer => name,
                        CommitField::AuthorEmail |
                        CommitField::CommitterEmail => email,
                        CommitField::AuthorTimestamp |
                        CommitField::CommitterTimestamp => timestamp,
                        CommitField::AuthorTimezone |
                        CommitField::CommitterTimezone => timezone,
                        _ => unreachable!(),
                    }));
                },
            }
        }

        Ok(None)
    }
}

pub fn get_commit_field_hash(commit: &[u8], field: CommitField) -> Result<Option<Hash>> {
    match get_commit_field(commit, field)? {
        Some(hex) => Ok(Some(Hash::from_hex(hex).ok_or(Error::InvalidObject)?)),
        None => Ok(None),
    }
}

pub struct CommitParentsIter<'a> {
    commit: &'a [u8],
    parent_index: usize,
}

impl<'a> CommitParentsIter<'a> {
    pub fn new(commit_object: &'a [u8]) -> CommitParentsIter<'a> {
        Self {
            commit: commit_object,
            parent_index: 0,
        }
    }

    pub fn next(&mut self) -> Result<Option<Hash>> {
        let field = CommitField::Parent(self.parent_index);
        if let Some(parent) = get_commit_field_hash(self.commit, field)? {
            self.parent_index += 1;
            Ok(Some(parent))
        } else {
            Ok(None)
        }
    }
}
