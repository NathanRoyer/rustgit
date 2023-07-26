use lmfu::{LiteMap, ArcStr};

use super::internals::{Result, Error, Hash};

pub type Directory = LiteMap<ArcStr, (Hash, Mode)>;

/// Filter for entries in a directory
#[derive(Copy, Clone, Debug)]
pub enum EntryType {
    Directory,
    File,
    All,
}

/// Types of files in directories
#[derive(Copy, Clone, Debug)]
pub enum FileType {
    RegularFile,
    GroupWriteableFile,
    ExecutableFile,
    SymbolicLink,
    Gitlink,
}

/// [`FileType`] with a `Directory` variant
#[derive(Copy, Clone, Debug)]
#[repr(u32)]
pub enum Mode {
    Directory = 0o040000,
    RegularFile = 0o100644,
    GroupWriteableFile = 0o100664,
    ExecutableFile = 0o100755,
    SymbolicLink = 0o120000,
    Gitlink = 0o160000,
}

impl From<FileType> for Mode {
    fn from(ft: FileType) -> Self {
        match ft {
            FileType::RegularFile => Self::RegularFile,
            FileType::GroupWriteableFile => Self::GroupWriteableFile,
            FileType::ExecutableFile => Self::ExecutableFile,
            FileType::SymbolicLink => Self::SymbolicLink,
            FileType::Gitlink => Self::Gitlink,
        }
    }
}

impl Mode {
    pub fn matches(self, entry_type: EntryType) -> bool {
        match self {
            Mode::Directory => match entry_type {
                EntryType::File => false,
                _ => true,
            },
            _ => match entry_type {
                EntryType::Directory => false,
                _ => true,
            },
        }
    }
}

pub struct Path<'a>(&'a str);

impl<'a> Path<'a> {
    pub fn new(string: &'a str) -> Path<'a> {
        Self(string)
    }

    pub fn dirs(&self) -> Result<impl Iterator<Item = &str>> {
        let iter = self.all();
        let num_subdirs = iter
            .clone()
            .count()
            .checked_sub(1)
            .ok_or(Error::PathError)?;

        Ok(iter.take(num_subdirs))
    }

    pub fn file(&self) -> Result<&str> {
        self.all().last().ok_or(Error::PathError)
    }

    pub fn all(&self) -> impl Iterator<Item = &str> + Clone {
        self.0.split('/').filter(|part| !part.is_empty())
    }
}
